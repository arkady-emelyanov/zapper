//! The shared drive: versioned artifacts that workers publish. Files are
//! copied out of the worker's VM and stored by content hash in the run's
//! `drive/` directory; the journal records each version.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zapper_policy::proto::{Frame, ShellRequest};
use zapper_policy::{parse_size, Firm, BOARD};

use super::{now, FirmRun};
use crate::daemon::session::{read_frame, Session};

const KINDS: [&str; 6] = ["document", "report", "code", "dataset", "deck", "board-pack"];
const MAX_FILES: usize = 2000;
const MAX_PATHS: usize = 64;

#[derive(Debug, Clone, Serialize)]
pub struct File {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Version {
    pub version: u32,
    pub created: f64,
    pub summary: String,
    /// What changed from the previous version.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub changes: String,
    pub files: Vec<File>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Artifact {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub author: String,
    /// `firm`, `board`, or names.
    pub audience: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    pub versions: Vec<Version>,
}

impl Artifact {
    pub fn visible_to(&self, who: &str) -> bool {
        if who == BOARD || who == self.author {
            return true;
        }
        match &self.audience {
            Value::String(s) if s == "firm" => true,
            Value::Array(names) => names.iter().any(|n| n == who),
            _ => false,
        }
    }

    fn summary(&self) -> Value {
        let last = self.versions.last();
        json!({
            "id": self.id, "title": self.title, "kind": self.kind, "author": self.author, "audience": self.audience,
            "thread": self.thread, "versions": self.versions.len(), "latest": last.map(|v| v.version),
            "summary": last.map(|v| v.summary.clone()), "updated": last.map(|v| v.created),
            "changes": last.map(|v| v.changes.clone()).filter(|c| !c.is_empty()),
            "history": self.versions.iter().map(|v| json!({ "version": v.version, "created": v.created, "changes": v.changes })).collect::<Vec<_>>(),
            "files": last.map(|v| v.files.len()), "size": last.map(|v| v.files.iter().map(|f| f.size).sum::<u64>()),
        })
    }
}

pub struct Drive {
    pub artifacts: BTreeMap<String, Artifact>,
    counter: u64,
    used: u64,
    artifact_limit: u64,
    drive_limit: u64,
}

fn sha256(path: &Path) -> Result<(String, u64)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        ctx.update(&buf[..n]);
    }
    let hex: String = ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    Ok((hex, size))
}

/// A guest path a worker may publish from or fetch into.
pub fn guest_path(p: &str, home: &str) -> Result<String> {
    let p = p.trim();
    if !p.starts_with('/') {
        bail!("{p:?}: give an absolute path");
    }
    if p.split('/').any(|c| c == ".." || c == ".") || p.contains('\0') || p.contains('\n') {
        bail!("{p:?}: no . or .. components");
    }
    let p = p.trim_end_matches('/');
    let ok = ["/work", home, "/tmp"].iter().any(|root| p == *root || p.starts_with(&format!("{root}/")));
    if !ok {
        bail!("{p:?}: only paths under /work, {home} or /tmp");
    }
    Ok(p.to_string())
}

/// Runs a command as root in the guest, feeding `input` to it and writing its
/// output to `out`; returns its exit code and stderr.
pub async fn guest_command<R, W>(s: &Session, argv: Vec<String>, input: Option<R>, mut out: W, limit: u64) -> Result<(i32, String)>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    let (stream, _) = s.open_shell(ShellRequest { id: 0, argv, tty: None, env: Default::default(), system: true }).await?;
    let (mut r, mut w) = stream.into_split();
    let feeder = tokio::spawn(async move {
        if let Some(mut input) = input {
            let mut buf = vec![0u8; 60 * 1024];
            loop {
                let n = match input.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let mut f = Vec::new();
                Frame::Data(buf[..n].to_vec()).write_to(&mut f).ok();
                if w.write_all(&f).await.is_err() {
                    return;
                }
            }
        }
        let mut f = Vec::new();
        Frame::Eof.write_to(&mut f).ok();
        let _ = w.write_all(&f).await;
        // Keep the write half open until the command exits.
        std::future::pending::<()>().await;
    });
    let mut stderr = String::new();
    let mut total = 0u64;
    let mut code = 255;
    while let Some(f) = read_frame(&mut r).await? {
        match f {
            Frame::Data(d) => {
                total += d.len() as u64;
                if total > limit {
                    feeder.abort();
                    bail!("more than {limit} bytes");
                }
                out.write_all(&d).await?;
            }
            Frame::Stderr(d) => {
                if stderr.len() < 4096 {
                    stderr.push_str(&String::from_utf8_lossy(&d));
                }
            }
            Frame::Exit(c) => {
                code = c;
                break;
            }
            _ => {}
        }
    }
    feeder.abort();
    out.flush().await?;
    Ok((code, stderr))
}

fn files_under(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let t = e.file_type()?;
            let p = e.path();
            if t.is_dir() {
                stack.push(p);
            } else if t.is_file() {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.push((rel, p));
                if out.len() > MAX_FILES {
                    bail!("more than {MAX_FILES} files");
                }
            }
            // Symlinks and special files are left out.
        }
    }
    out.sort();
    Ok(out)
}

pub struct PublishArgs {
    pub paths: Vec<String>,
    pub title: String,
    pub kind: String,
    pub summary: String,
    pub audience: Value,
    pub artifact: Option<String>,
    pub thread: Option<String>,
    /// What changed from the previous version; required for a new version.
    pub changes: String,
}

impl Drive {
    pub fn new(firm: &Firm) -> Result<Drive> {
        Ok(Drive {
            artifacts: BTreeMap::new(),
            counter: 0,
            used: 0,
            artifact_limit: parse_size(&firm.limits.artifact_size)?,
            drive_limit: parse_size(&firm.limits.drive_size)?,
        })
    }

    pub fn list(&self, who: &str, kind: Option<&str>, author: Option<&str>, query: Option<&str>) -> Vec<Value> {
        let q = query.map(str::to_ascii_lowercase);
        self.artifacts
            .values()
            .rev()
            .filter(|a| a.visible_to(who))
            .filter(|a| kind.is_none_or(|k| a.kind == k))
            .filter(|a| author.is_none_or(|x| a.author == x))
            .filter(|a| {
                q.as_ref().is_none_or(|q| a.title.to_ascii_lowercase().contains(q) || a.versions.last().is_some_and(|v| v.summary.to_ascii_lowercase().contains(q)))
            })
            .map(Artifact::summary)
            .collect()
    }

    /// `a-3` or `a-3@v2` (also with a `drive:` prefix) → artifact and version.
    pub fn resolve(&self, r: &str, who: &str) -> Result<(&Artifact, &Version)> {
        let r = r.trim().trim_start_matches("drive:");
        let (id, ver) = match r.split_once('@') {
            Some((id, v)) => (id, Some(v.trim_start_matches('v').parse::<u32>().map_err(|_| anyhow!("bad version in {r}"))?)),
            None => (r, None),
        };
        let a = self.artifacts.get(id).filter(|a| a.visible_to(who)).ok_or_else(|| anyhow!("no artifact {id}"))?;
        let v = match ver {
            Some(n) => a.versions.iter().find(|v| v.version == n).ok_or_else(|| anyhow!("{id} has no version {n}"))?,
            None => a.versions.last().ok_or_else(|| anyhow!("{id} has no versions"))?,
        };
        Ok((a, v))
    }
}

fn audience(v: &Value) -> Result<Value> {
    match v {
        Value::Null => Ok(json!("firm")),
        Value::String(s) if s == "firm" || s == "board" => Ok(v.clone()),
        Value::Array(a) if a.iter().all(|x| x.as_str().is_some_and(zapper_policy::valid_worker_name)) && a.len() <= 64 => Ok(v.clone()),
        _ => bail!("audience is \"firm\", \"board\", or a list of names"),
    }
}

/// Copies files out of the worker's VM into the drive.
pub async fn publish(run: &Arc<FirmRun>, worker: &str, s: &Session, a: PublishArgs) -> Result<Value> {
    if !KINDS.contains(&a.kind.as_str()) {
        bail!("kind is one of {}", KINDS.join(", "));
    }
    if a.title.trim().is_empty() || a.title.len() > 200 {
        bail!("give a title of up to 200 characters");
    }
    if a.summary.len() > 4000 {
        bail!("keep the summary under 4000 characters");
    }
    if a.changes.len() > 2000 {
        bail!("keep the note of changes under 2000 characters");
    }
    if a.artifact.is_some() && a.changes.trim().is_empty() {
        bail!("a new version needs `changes`: what changed from the previous version, for the people who read it");
    }
    if a.paths.is_empty() || a.paths.len() > MAX_PATHS {
        bail!("give 1 to {MAX_PATHS} paths");
    }
    let aud = audience(&a.audience)?;
    if let Some(id) = &a.artifact {
        let d = run.drive.lock().unwrap();
        let art = d.artifacts.get(id).ok_or_else(|| anyhow!("no artifact {id}"))?;
        if art.author != worker {
            bail!("{id} is {}'s; publish your own artifact, or ask them to add a version", art.author);
        }
    }
    let home = format!("/home/{}", s.resolved.agent.user);
    let mut argv = vec!["tar".to_string(), "-cf".into(), "-".into()];
    let mut names = std::collections::BTreeSet::new();
    for p in &a.paths {
        let p = guest_path(p, &home)?;
        let (parent, base) = p.rsplit_once('/').unwrap();
        let parent = if parent.is_empty() { "/" } else { parent };
        if !names.insert(base.to_string()) {
            bail!("two paths are named {base}; publish them separately or from one directory");
        }
        argv.extend(["-C".into(), parent.into(), format!("./{base}")]);
    }
    let limit = run.drive.lock().unwrap().artifact_limit;
    let tmp = run.dir.join("drive").join(format!("tmp-{}", crate::daemon::random_id(8)));
    std::fs::create_dir_all(&tmp)?;
    let result = async {
        // The archive comes from the guest: it is parsed here, keeping only
        // regular files with clean relative paths, rather than unpacked by
        // a tool that would also create links and devices.
        let archive = tmp.join("archive.tar");
        let file = tokio::fs::File::create(&archive).await?;
        let (code, err) = guest_command(s, argv, None::<tokio::io::Empty>, file, limit + limit / 4 + (1 << 20)).await.context("copying out of the VM")?;
        if code != 0 {
            bail!("reading the files failed: {}", err.trim());
        }
        let out = tmp.join("files");
        std::fs::create_dir_all(&out)?;
        untar(&archive, &out, limit)?;
        let _ = std::fs::remove_file(&archive);
        let tmp = out;
        let files = files_under(&tmp)?;
        if files.is_empty() {
            bail!("no regular files under those paths");
        }
        let mut entries = Vec::new();
        let mut total = 0;
        for (rel, path) in files {
            let (hash, size) = sha256(&path)?;
            total += size;
            if total > limit {
                bail!("the artifact is larger than {}", run.config.limits.artifact_size);
            }
            let blob = run.dir.join("drive").join(&hash);
            if !blob.exists() {
                std::fs::rename(&path, &blob)?;
            }
            entries.push(File { path: rel, sha256: hash, size });
        }
        Ok::<_, anyhow::Error>((entries, total))
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    let (files, total) = result?;
    let mut d = run.drive.lock().unwrap();
    if d.used + total > d.drive_limit {
        bail!("the shared drive is full ({} of {})", d.used, run.config.limits.drive_size);
    }
    d.used += total;
    let id = match &a.artifact {
        Some(id) => id.clone(),
        None => {
            d.counter += 1;
            format!("a-{}", d.counter)
        }
    };
    let art = d.artifacts.entry(id.clone()).or_insert_with(|| Artifact {
        id: id.clone(),
        title: a.title.clone(),
        kind: a.kind.clone(),
        author: worker.to_string(),
        audience: aud.clone(),
        thread: a.thread.clone(),
        versions: vec![],
    });
    art.title = a.title.clone();
    art.audience = aud;
    if a.thread.is_some() {
        art.thread = a.thread.clone();
    }
    let version = art.versions.len() as u32 + 1;
    let changes = if version == 1 && a.changes.trim().is_empty() { "First version.".to_string() } else { a.changes.trim().to_string() };
    art.versions.push(Version { version, created: now(), summary: a.summary.clone(), changes: changes.clone(), files: files.clone() });
    let entry = json!({
        "artifact": id, "version": version, "title": art.title, "kind": art.kind, "author": worker,
        "audience": art.audience, "thread": art.thread, "summary": a.summary, "changes": changes, "files": files,
    });
    drop(d);
    run.journal.append("published", entry);
    Ok(json!({ "artifact": id, "version": version, "ref": format!("drive:{id}@v{version}"), "files": files.len(), "size": total }))
}

/// Copies an artifact into the worker's VM.
pub async fn fetch(run: &Arc<FirmRun>, worker: &str, s: &Session, r: &str, dest: &str) -> Result<Value> {
    let home = format!("/home/{}", s.resolved.agent.user);
    let dest = guest_path(dest, &home)?;
    let (id, version, files) = {
        let d = run.drive.lock().unwrap();
        let (a, v) = d.resolve(r, worker)?;
        (a.id.clone(), v.version, v.files.clone())
    };
    let tmp = run.dir.join("drive").join(format!("tmp-{}", crate::daemon::random_id(8)));
    let result = async {
        for f in &files {
            let to = tmp.join(&f.path);
            std::fs::create_dir_all(to.parent().unwrap())?;
            std::fs::copy(run.dir.join("drive").join(&f.sha256), &to)?;
        }
        let mut tar = tokio::process::Command::new("tar")
            .args(["-cf", "-", "-C"])
            .arg(&tmp)
            .arg(".")
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let stdout = tar.stdout.take().unwrap();
        let script = format!(
            "mkdir -p '{dest}' && tar --no-same-owner -xf - -C '{dest}' && chown -R 1000:1000 '{dest}'",
            dest = dest.replace('\'', "")
        );
        let (code, err) = guest_command(s, vec!["sh".into(), "-c".into(), script], Some(stdout), tokio::io::sink(), 1 << 20).await?;
        let _ = tar.wait().await;
        if code != 0 {
            bail!("writing into {dest} failed: {}", err.trim());
        }
        Ok(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    result?;
    run.journal.append("fetched", json!({ "artifact": id, "version": version, "worker": worker, "dest": dest }));
    Ok(json!({ "artifact": id, "version": version, "dest": dest, "files": files.iter().map(|f| f.path.clone()).collect::<Vec<_>>() }))
}

/// Finds a published file in a run's journal, for serving it: its hash.
pub fn find_file(run_dir: &Path, artifact: &str, version: u32, path: &str) -> Result<(String, u64)> {
    let text = std::fs::read_to_string(run_dir.join("journal.jsonl"))?;
    for line in text.lines().rev() {
        if !line.contains("\"published\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "published" || v["artifact"] != artifact || v["version"] != version {
            continue;
        }
        for f in v["files"].as_array().into_iter().flatten() {
            if f["path"] == path {
                let sha = f["sha256"].as_str().unwrap_or("").to_string();
                if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                    bail!("bad hash");
                }
                return Ok((sha, f["size"].as_u64().unwrap_or(0)));
            }
        }
    }
    bail!("no file {path} in {artifact}@v{version}")
}

/// A path from an archive, made safe: relative, without `.`, `..` or empty
/// components.
fn clean(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains('\0')) || parts.len() > 32 {
        return None;
    }
    Some(parts.join("/"))
}

fn octal(field: &[u8]) -> u64 {
    let s: String = field.iter().take_while(|b| **b != 0).map(|b| *b as char).collect();
    u64::from_str_radix(s.trim(), 8).unwrap_or(0)
}

/// Extracts the regular files of a tar archive (ustar, GNU long names, pax
/// paths) into `out`; links, devices and anything else are skipped.
pub fn untar(archive: &Path, out: &Path, limit: u64) -> Result<usize> {
    use std::io::{Read, Write};
    let mut f = std::io::BufReader::new(std::fs::File::open(archive)?);
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;
    let mut pax_path: Option<String> = None;
    let mut count = 0;
    let mut total = 0u64;
    loop {
        if f.read_exact(&mut header).is_err() || header.iter().all(|b| *b == 0) {
            break;
        }
        let size = octal(&header[124..136]);
        let padded = size.div_ceil(512) * 512;
        let kind = header[156];
        let field = |r: std::ops::Range<usize>| String::from_utf8_lossy(&header[r]).trim_end_matches('\0').to_string();
        let mut name = field(0..100);
        let prefix = field(345..500);
        if &header[257..262] == b"ustar" && !prefix.is_empty() {
            name = format!("{prefix}/{name}");
        }
        match kind {
            b'L' | b'x' => {
                if size > 64 * 1024 {
                    bail!("archive metadata too large");
                }
                let mut data = vec![0u8; padded as usize];
                f.read_exact(&mut data)?;
                data.truncate(size as usize);
                if kind == b'L' {
                    long_name = Some(String::from_utf8_lossy(&data).trim_end_matches('\0').to_string());
                } else {
                    for rec in String::from_utf8_lossy(&data).lines() {
                        if let Some((_, kv)) = rec.split_once(' ') {
                            if let Some(p) = kv.strip_prefix("path=") {
                                pax_path = Some(p.to_string());
                            }
                        }
                    }
                }
                continue;
            }
            _ => {}
        }
        let name = pax_path.take().or(long_name.take()).unwrap_or(name);
        if matches!(kind, b'0' | 0 | b'7') {
            total += size;
            if total > limit {
                bail!("more than {limit} bytes");
            }
            count += 1;
            if count > MAX_FILES {
                bail!("more than {MAX_FILES} files");
            }
            if let Some(rel) = clean(&name) {
                let to = out.join(&rel);
                std::fs::create_dir_all(to.parent().unwrap())?;
                let mut w = std::fs::OpenOptions::new().write(true).create_new(true).open(&to)?;
                let mut left = size;
                let mut buf = vec![0u8; 64 * 1024];
                while left > 0 {
                    let n = (left as usize).min(buf.len());
                    f.read_exact(&mut buf[..n])?;
                    w.write_all(&buf[..n])?;
                    left -= n as u64;
                }
                let mut pad = vec![0u8; (padded - size) as usize];
                f.read_exact(&mut pad)?;
                continue;
            }
        }
        // Skip the entry's data.
        std::io::copy(&mut (&mut f).take(padded), &mut std::io::sink())?;
    }
    Ok(count)
}

/// Content type for serving a published file.
pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" | "txt" | "log" | "csv" | "tsv" | "json" | "yaml" | "yml" | "toml" | "rs" | "py" | "js" | "ts" | "go" | "html" | "css" | "sh" | "sql" => {
            "text/plain; charset=utf-8"
        }
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untar_keeps_regular_files_only() {
        let dir = std::env::temp_dir().join(format!("zapper-untar-{}", crate::daemon::random_id(6)));
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.md"), "hello").unwrap();
        std::fs::write(src.join("sub/b.txt"), "x".repeat(700)).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", src.join("link")).unwrap();
        let long = "n".repeat(120);
        std::fs::write(src.join(&long), "long").unwrap();
        let archive = dir.join("a.tar");
        let ok = std::process::Command::new("tar").arg("-cf").arg(&archive).arg("-C").arg(&src).arg(".").status().unwrap();
        assert!(ok.success());
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert_eq!(untar(&archive, &out, 1 << 20).unwrap(), 3);
        assert_eq!(std::fs::read_to_string(out.join("a.md")).unwrap(), "hello");
        assert_eq!(std::fs::read_to_string(out.join("sub/b.txt")).unwrap().len(), 700);
        assert_eq!(std::fs::read_to_string(out.join(&long)).unwrap(), "long");
        assert!(std::fs::symlink_metadata(out.join("link")).is_err());
        assert!(untar(&archive, &dir.join("out2"), 100).is_err(), "limit");
        assert_eq!(clean("../etc/passwd"), None);
        assert_eq!(clean("/abs/x"), Some("abs/x".into()));
        assert_eq!(clean("./a//b"), Some("a/b".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn guest_paths() {
        assert_eq!(guest_path("/work/a.md", "/home/agent").unwrap(), "/work/a.md");
        assert_eq!(guest_path("/home/agent/x/", "/home/agent").unwrap(), "/home/agent/x");
        assert!(guest_path("/etc/shadow", "/home/agent").is_err());
        assert!(guest_path("/work/../etc", "/home/agent").is_err());
        assert!(guest_path("work/a", "/home/agent").is_err());
        assert!(guest_path("/workshop", "/home/agent").is_err());
    }
}
