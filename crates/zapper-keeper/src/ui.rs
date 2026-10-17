//! The keeper's web UI: read-only. Its status, the record's files (with how
//! many lines each has and when it was last signed), and verifying any of
//! them against their chains. It listens on a loopback address, wants the
//! token from `<dir>/keeper-ui` (readable by the writer's group), and takes
//! only GETs with a local `Host`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::{chain_path, free_bytes, valid_stream, verify, ChainLine, Keeper};

const PAGE: &str = include_str!("ui.html");

pub struct Ui {
    keeper: Arc<Keeper>,
    writer: u32,
    token: String,
    port: u16,
    started: f64,
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn random_token() -> Result<String> {
    let mut b = [0u8; 18];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Starts the UI on `addr` (a loopback address) and writes its URL, with
/// the token, to `<dir>/keeper-ui`.
pub fn start(keeper: Arc<Keeper>, addr: &str, writer: u32, group: Option<u32>) -> Result<()> {
    let listener = TcpListener::bind(addr).with_context(|| format!("the keeper's UI on {addr}"))?;
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        bail!("the keeper's UI listens on a loopback address only, not {local}");
    }
    let token = random_token()?;
    let url = format!("http://{local}/?token={token}\n");
    let path = keeper.root().join("keeper-ui");
    let tmp = keeper.root().join(".keeper-ui.tmp");
    std::fs::write(&tmp, &url)?;
    std::fs::set_permissions(
        &tmp,
        std::os::unix::fs::PermissionsExt::from_mode(if group.is_some() { 0o640 } else { 0o600 }),
    )?;
    if let Some(g) = group {
        crate::give(&std::fs::File::open(&tmp)?, g)?;
    }
    std::fs::rename(&tmp, &path)?;
    eprintln!(
        "zapper-keeper: UI at http://{local}/ (the URL with its token is in {})",
        path.display()
    );
    let ui = Arc::new(Ui {
        keeper,
        writer,
        token,
        port: local.port(),
        started: now(),
    });
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let ui = ui.clone();
            std::thread::spawn(move || {
                let _ = ui.handle(conn);
            });
        }
    });
    Ok(())
}

fn respond(mut s: TcpStream, status: &str, ctype: &str, body: &[u8]) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    Ok(())
}

fn json_reply(s: TcpStream, v: &Value) -> Result<()> {
    respond(s, "200 OK", "application/json", v.to_string().as_bytes())
}

fn query(q: &str, key: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| decode(v))
    })
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'+', _) => out.push(b' '),
            (b'%', Some(v)) => {
                out.push(v);
                i += 2;
            }
            (c, _) => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A path under the record: plain names, as streams are.
fn valid_dir(p: &str) -> bool {
    p.is_empty()
        || p.split('/').all(|c| {
            !c.is_empty()
                && !c.starts_with('.')
                && c.len() <= 128
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

/// The last entry and checkpoint of a chain, from its tail.
fn chain_tail(path: &Path) -> (u64, Option<(u64, f64)>) {
    let Ok(mut f) = std::fs::File::open(path) else {
        return (0, None);
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(64 * 1024);
    let _ = std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(start));
    let mut buf = String::new();
    let _ = f.read_to_string(&mut buf);
    let (mut seq, mut signed) = (0, None);
    for line in buf.lines() {
        let Ok(c) = serde_json::from_str::<ChainLine>(line) else {
            continue;
        };
        if c.sig.is_some() {
            signed = Some((c.seq, c.ts.unwrap_or(0.0)));
        } else {
            seq = c.seq;
        }
    }
    (seq, signed)
}

impl Ui {
    fn handle(&self, conn: TcpStream) -> Result<()> {
        conn.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut r = BufReader::new(conn.try_clone()?.take(16 * 1024));
        let mut first = String::new();
        r.read_line(&mut first)?;
        let mut host = String::new();
        let mut token_header = String::new();
        loop {
            let mut l = String::new();
            if r.read_line(&mut l)? == 0 || l == "\r\n" || l == "\n" {
                break;
            }
            let (k, v) = l.split_once(':').unwrap_or(("", ""));
            match k.trim().to_ascii_lowercase().as_str() {
                "host" => host = v.trim().to_string(),
                "x-keeper-token" => token_header = v.trim().to_string(),
                _ => {}
            }
        }
        let parts: Vec<&str> = first.split_whitespace().collect();
        if parts.len() < 2 || parts[0] != "GET" {
            return respond(conn, "405 Method Not Allowed", "text/plain", b"read only\n");
        }
        // A local Host header: no DNS rebinding.
        let local = [
            format!("127.0.0.1:{}", self.port),
            format!("localhost:{}", self.port),
            format!("[::1]:{}", self.port),
        ];
        if !local.contains(&host) {
            return respond(conn, "403 Forbidden", "text/plain", b"bad Host header\n");
        }
        let (path, q) = parts[1].split_once('?').unwrap_or((parts[1], ""));
        if path == "/" {
            // The page itself carries no data; the token in the URL is
            // passed to the API by the page.
            return respond(conn, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes());
        }
        if token_header != self.token {
            return respond(conn, "401 Unauthorized", "text/plain", b"token required\n");
        }
        match path {
            "/api/status" => json_reply(conn, &self.status()),
            "/api/list" => {
                let dir = query(q, "dir").unwrap_or_default();
                match self.list(&dir) {
                    Ok(v) => json_reply(conn, &v),
                    Err(e) => respond(
                        conn,
                        "400 Bad Request",
                        "text/plain",
                        format!("{e:#}\n").as_bytes(),
                    ),
                }
            }
            "/api/verify" => {
                let p = query(q, "path").unwrap_or_default();
                match self.verify(&p) {
                    Ok(v) => json_reply(conn, &v),
                    Err(e) => respond(
                        conn,
                        "400 Bad Request",
                        "text/plain",
                        format!("{e:#}\n").as_bytes(),
                    ),
                }
            }
            _ => respond(conn, "404 Not Found", "text/plain", b"not found\n"),
        }
    }

    fn status(&self) -> Value {
        let pk = self.keeper.public_key();
        json!({
            "dir": self.keeper.root().display().to_string(), "writer_uid": self.writer, "public_key": pk, "fingerprint": pk.get(..16).unwrap_or(""),
            "free_bytes": free_bytes(self.keeper.root()), "min_free": self.keeper.min_free, "started": self.started, "now": now(),
        })
    }

    fn list(&self, dir: &str) -> Result<Value> {
        let dir = dir.trim_matches('/');
        if !valid_dir(dir) {
            bail!("{dir:?} is not a directory of the record");
        }
        let root = self.keeper.root().join(dir);
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for e in std::fs::read_dir(&root)
            .with_context(|| format!("no directory {dir:?}"))?
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.ends_with(".chain") {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                dirs.push(json!({ "name": name }));
            } else if name.ends_with(".jsonl") || name.ends_with(".cast") {
                let (lines, signed) = chain_tail(&chain_path(&p));
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                let modified = e
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64());
                files.push(json!({ "name": name, "size": size, "lines": lines, "signed": signed.map(|s| s.0), "signed_at": signed.map(|s| s.1), "modified": modified }));
            }
        }
        dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({ "dir": dir, "dirs": dirs, "files": files }))
    }

    /// Verifies a file, or every file under a directory.
    fn verify(&self, p: &str) -> Result<Value> {
        let p = p.trim_matches('/');
        if !valid_dir(p) {
            bail!("{p:?} is not in the record");
        }
        let pk = self.keeper.public_key();
        let mut out = Vec::new();
        let mut todo: Vec<PathBuf> = vec![self.keeper.root().join(p)];
        let mut n = 0;
        while let Some(x) = todo.pop() {
            if x.is_dir() {
                for e in std::fs::read_dir(&x)?.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !name.starts_with('.') {
                        todo.push(e.path());
                    }
                }
                continue;
            }
            let rel = x
                .strip_prefix(self.keeper.root())
                .ok()
                .and_then(|r| r.to_str())
                .unwrap_or("")
                .to_string();
            if !valid_stream(&rel) {
                continue;
            }
            n += 1;
            if n > 10_000 {
                bail!("more than 10000 files; verify a smaller directory");
            }
            out.push(match verify(&x, &rel, &pk) {
                Ok(r) => json!({ "file": rel, "ok": true, "lines": r.lines, "signed": r.signed, "checkpoints": r.checkpoints }),
                Err(e) => json!({ "file": rel, "ok": false, "error": format!("{e:#}") }),
            });
        }
        out.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
        let ok = out.iter().all(|x| x["ok"] == json!(true));
        Ok(json!({ "path": p, "ok": ok, "files": out }))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn decoding_and_dirs() {
        assert_eq!(super::decode("firms%2Facme+x"), "firms/acme x");
        assert!(super::valid_dir("firms/acme/runs"));
        assert!(super::valid_dir(""));
        assert!(!super::valid_dir("../etc"));
        assert!(!super::valid_dir("a/.x"));
    }
}
