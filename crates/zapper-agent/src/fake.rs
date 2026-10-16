//! A fake Claude Code for tests: speaks the same stream-json on stdin and
//! stdout as `claude -p --input-format stream-json --output-format
//! stream-json`, and plays a script instead of thinking. Tools are called
//! through the office MCP server named in `~/.claude.json`, as Claude would.
//!
//! The script is YAML:
//!
//! ```yaml
//! rules:                      # the first rule that matches a wake runs
//!   - when: { from: board, kind: assignment }   # also: subject, contains, once
//!     do:
//!       - mail_send: { to: [ada], subject: Build it, body: "...", kind: assignment }
//!       - shell: "echo done > /work/out.md"
//!       - drive_publish: { paths: [/work/out.md], title: Out, kind: document, summary: s }
//!       - mail_reply: { message: $message, body: "Done: $artifact", kind: report, close: true }
//!       - say: some text
//!       - sleep: 1
//! ```
//!
//! `$message`, `$thread`, `$from`, `$subject` come from the wake's first
//! message, `$request` from its first access request, `$artifact` from the
//! last publish, `$last` is the last tool result's text.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

struct Mcp {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start() -> Result<Mcp> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let state: Value = serde_json::from_slice(&std::fs::read(format!("{home}/.claude.json"))?)?;
        let server = &state["mcpServers"]["office"];
        let cmd = server["command"].as_str().ok_or_else(|| anyhow!("no office MCP server in ~/.claude.json"))?;
        let args: Vec<String> = server["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect();
        let mut child = Command::new(cmd).args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().context("starting the office MCP server")?;
        let mut m = Mcp { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), _child: child, next: 1 };
        m.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "fake-claude", "version": "0" } }))?;
        writeln!(m.stdin, "{}", json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(m)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        self.stdin.flush()?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                bail!("the MCP server exited");
            }
            let v: Value = serde_json::from_str(&line)?;
            if v["id"] == json!(id) {
                if let Some(e) = v.get("error") {
                    bail!("{e}");
                }
                return Ok(v["result"].clone());
            }
        }
    }
}

/// Fields of the wake prompt the script can use.
#[derive(Default, Debug)]
struct Wake {
    text: String,
    message: String,
    thread: String,
    from: String,
    kind: String,
    subject: String,
    request: String,
}

fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.find(marker).map(|i| &text[i + marker.len()..])
}

fn word(s: &str) -> String {
    s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

fn parse_wake(text: &str) -> Wake {
    let mut w = Wake { text: text.to_string(), ..Default::default() };
    if let Some(rest) = after(text, "\nMessage: ").or_else(|| after(text, "Message: ")) {
        w.message = word(rest);
    }
    if let Some(rest) = after(text, "Thread: \"") {
        if let Some(end) = rest.find("\" (") {
            w.subject = rest[..end].to_string();
            w.thread = word(&rest[end + 3..]);
        }
    }
    if let Some(rest) = after(text, "\nFrom: ") {
        w.from = word(rest);
    }
    if let Some(rest) = after(text, "\nKind: ") {
        w.kind = word(rest);
    }
    if let Some(rest) = after(text, "Access request ") {
        w.request = word(rest);
    }
    w
}

fn matches(when: &Value, w: &Wake) -> bool {
    let field = |k: &str| when[k].as_str();
    field("from").is_none_or(|f| f == w.from)
        && field("kind").is_none_or(|k| k == w.kind)
        && field("subject").is_none_or(|s| w.subject.contains(s))
        && field("contains").is_none_or(|c| w.text.contains(c))
        && field("request").is_none_or(|_| !w.request.is_empty())
}

fn substitute(v: &Value, vars: &[(&str, String)]) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for (k, val) in vars {
                s = s.replace(k, val);
            }
            json!(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, vars)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), substitute(x, vars))).collect()),
        other => other.clone(),
    }
}

fn emit(v: Value) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

pub fn run(script: &str) -> i32 {
    match play(script) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("fake-claude: {e:#}");
            1
        }
    }
}

fn play(script: &str) -> Result<()> {
    let script: Value = serde_yaml::from_str(&std::fs::read_to_string(script).with_context(|| format!("reading {script}"))?)?;
    let rules = script["rules"].as_array().cloned().unwrap_or_default();
    let session = format!("fake-{}", std::process::id());
    let mut mcp: Option<Mcp> = None;
    let mut fired: HashSet<usize> = HashSet::new();
    let mut artifact = String::new();
    let mut turns = 0u64;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if msg["type"] != "user" {
            continue;
        }
        let started = Instant::now();
        turns += 1;
        let text: String = match &msg["message"]["content"] {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        };
        emit(json!({ "type": "system", "subtype": "init", "session_id": session, "tools": [], "model": "fake" }));
        let wake = parse_wake(&text);
        let rule = rules.iter().enumerate().find(|(i, r)| !(r["when"]["once"] == json!(true) && fired.contains(i)) && matches(&r["when"], &wake));
        let mut last = String::new();
        let mut errors = 0;
        if let Some((i, rule)) = rule {
            fired.insert(i);
            for action in rule["do"].as_array().into_iter().flatten() {
                let vars = [
                    ("$message", wake.message.clone()),
                    ("$thread", wake.thread.clone()),
                    ("$from", wake.from.clone()),
                    ("$subject", wake.subject.clone()),
                    ("$request", wake.request.clone()),
                    ("$artifact", artifact.clone()),
                    ("$last", last.clone()),
                ];
                let Some((name, args)) = action.as_object().and_then(|o| o.iter().next()) else { continue };
                let args = substitute(args, &vars);
                match name.as_str() {
                    "say" => emit(json!({ "type": "assistant", "session_id": session, "message": { "role": "assistant", "content": [{ "type": "text", "text": args }] } })),
                    "sleep" => std::thread::sleep(std::time::Duration::from_secs_f64(args.as_f64().unwrap_or(1.0))),
                    "shell" => {
                        let cmd = args.as_str().unwrap_or("true");
                        emit(json!({ "type": "assistant", "session_id": session, "message": { "role": "assistant", "content": [{ "type": "tool_use", "id": "t", "name": "Bash", "input": { "command": cmd } }] } }));
                        let out = Command::new("sh").args(["-c", cmd]).output();
                        last = match out {
                            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr),
                            Err(e) => e.to_string(),
                        };
                        emit(json!({ "type": "user", "session_id": session, "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t", "content": last }] } }));
                    }
                    tool => {
                        emit(json!({ "type": "assistant", "session_id": session, "message": { "role": "assistant", "content": [{ "type": "tool_use", "id": "t", "name": format!("mcp__office__{tool}"), "input": args }] } }));
                        if mcp.is_none() {
                            mcp = Some(Mcp::start()?);
                        }
                        let r = mcp.as_mut().unwrap().request("tools/call", json!({ "name": tool, "arguments": args }))?;
                        last = r["content"][0]["text"].as_str().unwrap_or("").to_string();
                        if r["isError"] == json!(true) {
                            errors += 1;
                        } else if tool == "drive_publish" {
                            if let Ok(v) = serde_json::from_str::<Value>(&last) {
                                artifact = v["ref"].as_str().unwrap_or("").to_string();
                            }
                        }
                        emit(json!({ "type": "user", "session_id": session, "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t", "content": last, "is_error": r["isError"] }] } }));
                    }
                }
            }
        }
        emit(json!({
            "type": "result", "subtype": "success", "is_error": false, "session_id": session, "num_turns": turns,
            "duration_ms": started.elapsed().as_millis() as u64, "result": format!("played; {errors} tool errors"),
            "total_cost_usd": 0.0, "usage": { "input_tokens": 100, "output_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0 },
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_wake() {
        let text = "New mail: 1 message(s).\n\n── 1 of 1 ──\nThread: \"Landing page v1\" (t-3)\nMessage: m-7\nFrom: cto (CTO, your manager)\nKind: assignment · Expects: done\n";
        let w = parse_wake(text);
        assert_eq!((w.message.as_str(), w.thread.as_str(), w.from.as_str(), w.kind.as_str(), w.subject.as_str()), ("m-7", "t-3", "cto", "assignment", "Landing page v1"));
        assert!(matches(&json!({ "from": "cto", "kind": "assignment" }), &w));
        assert!(!matches(&json!({ "from": "board" }), &w));
        let r = parse_wake("Access request r-12 — decision needed.");
        assert_eq!(r.request, "r-12");
        assert_eq!(substitute(&json!({ "m": "$message!" }), &[("$message", "m-7".into())]), json!({ "m": "m-7!" }));
    }
}
