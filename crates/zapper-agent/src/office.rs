//! `office`: a firm worker's way to the post office, over the host's office
//! port. As `office mcp` it is the MCP server Claude Code loads; otherwise a
//! command line for scripts and people.
//!
//! The host knows which worker is asking from the VM the connection comes
//! from, and checks the role on every request; this side only translates.

use std::io::{BufRead, BufReader, Read, Write};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use zapper_policy::proto::{HOST_CID, MAX_OFFICE_LINE, OFFICE_PORT};

use crate::sys;

/// Sends one request to the post office and returns its answer.
pub fn call(op: &str, args: &Value) -> Result<Value> {
    let mut conn = sys::vsock_connect(HOST_CID, OFFICE_PORT)
        .context("connecting to the post office (is this a firm worker?)")?;
    let mut line = serde_json::to_vec(&json!({ "op": op, "args": args }))?;
    line.push(b'\n');
    if line.len() > MAX_OFFICE_LINE {
        bail!("request longer than {MAX_OFFICE_LINE} bytes");
    }
    conn.write_all(&line)?;
    let mut reply = Vec::new();
    BufReader::new(conn)
        .take(16 << 20)
        .read_until(b'\n', &mut reply)?;
    let v: Value = serde_json::from_slice(&reply).context("reading the post office's answer")?;
    match (v.get("ok"), v.get("error")) {
        (Some(ok), _) => Ok(ok.clone()),
        (_, Some(e)) => bail!("{}", e.as_str().unwrap_or("error")),
        _ => bail!("unexpected answer: {v}"),
    }
}

const USAGE: &str = "usage: office tools | office OP [JSON] | office OP --key value ...
       office mcp     (the MCP server Claude Code runs)
       office stop-hook  (Claude Code's Stop hook: mail that arrived during the turn)
examples:
  office desk
  office mail_send --to cto --subject 'Plan' --body 'Draft attached' --kind fyi
  office mail_reply '{\"message\": \"m-3\", \"body\": \"done\", \"kind\": \"report\", \"close\": true}'";

/// `--key value` pairs into a JSON object; values that parse as JSON are
/// taken as such, comma lists for `to`, `cc`, `paths`, `attachments`.
fn flags(args: &[String]) -> Result<Value> {
    let mut obj = serde_json::Map::new();
    let mut i = 0;
    while i < args.len() {
        let Some(k) = args[i].strip_prefix("--") else {
            bail!("unexpected {:?}\n{USAGE}", args[i])
        };
        let v = args.get(i + 1).cloned().unwrap_or_else(|| "true".into());
        let value = if ["to", "cc", "paths", "attachments", "methods"].contains(&k) {
            json!(v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>())
        } else {
            serde_json::from_str::<Value>(&v)
                .ok()
                .filter(|x| !x.is_string())
                .unwrap_or(json!(v))
        };
        obj.insert(k.to_string(), value);
        i += 2;
    }
    Ok(Value::Object(obj))
}

pub fn cli(args: &[String]) -> i32 {
    let result = (|| -> Result<Value> {
        match args.first().map(String::as_str) {
            None | Some("-h") | Some("--help") => {
                println!("{USAGE}");
                Ok(Value::Null)
            }
            Some("mcp") => {
                mcp()?;
                Ok(Value::Null)
            }
            Some("stop-hook") => {
                stop_hook();
                Ok(Value::Null)
            }
            Some(op) => {
                let rest = &args[1..];
                let a = match rest.first() {
                    Some(j) if j.trim_start().starts_with('{') => {
                        serde_json::from_str(j).context("JSON arguments")?
                    }
                    _ => flags(rest)?,
                };
                call(op, &a)
            }
        }
    })();
    match result {
        Ok(Value::Null) => 0,
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("office: {e:#}");
            1
        }
    }
}

/// Claude Code's Stop hook: if mail arrived during the turn, block the stop
/// and hand it over as the reason. Any failure lets Claude stop.
fn stop_hook() {
    let mut input = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    if let Ok(v) = call("stop_check", &json!({})) {
        if v["block"] == json!(true) {
            println!("{}", json!({ "decision": "block", "reason": v["reason"] }));
        }
    }
}

/// The MCP server: JSON-RPC 2.0, one message per line on stdin and stdout.
fn mcp() -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut tools: Option<Value> = None;
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg["method"].as_str().unwrap_or("");
        let result: std::result::Result<Value, (i64, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "office", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "The office: mail with colleagues, your desk, the org chart, the shared drive. See the office-mail and office-publish skills.",
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                if tools.is_none() {
                    tools = call("tools", &json!({})).ok();
                }
                match &tools {
                    Some(t) => Ok(json!({ "tools": t })),
                    None => Err((-32603, "the post office is unreachable".into())),
                }
            }
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                Ok(match call(name, &args) {
                    Ok(v) => {
                        json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }], "isError": false })
                    }
                    Err(e) => {
                        json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })
                    }
                })
            }
            _ => Err((-32601, format!("method {method} not found"))),
        };
        let reply = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        };
        let mut b = serde_json::to_vec(&reply)?;
        b.push(b'\n');
        out.write_all(&b)?;
        out.flush()?;
    }
    Ok(())
}
