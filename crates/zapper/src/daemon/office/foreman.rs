//! The foreman: drives one worker's Claude Code headless.
//!
//! Claude Code runs in the worker's VM as `claude -p --input-format
//! stream-json --output-format stream-json`, over a terminal channel without
//! a tty. Mail is delivered as a user message (the wake prompt) when the
//! worker is idle, and queued while it works; a turn ends with the `result`
//! event. Every event goes to the worker's transcript on disk.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;
use zapper_policy::proto::{Frame, ShellRequest};

use super::journal::{Journal, MAX_LINE};
use super::{now, prompts, security, FirmRun, RunState, WorkerRt, WorkerState};
use crate::daemon::session::{read_frame, Session};
use crate::daemon::Daemon;

/// Restarts allowed within ten minutes before the worker is marked broken.
const MAX_RESTARTS: usize = 5;

enum Out {
    /// A turn ended.
    Result(Value),
    /// The process exited.
    Exit(Option<i32>),
}

struct Proc {
    stdin: OwnedWriteHalf,
    events: mpsc::Receiver<Out>,
}

impl Proc {
    async fn send(&mut self, text: &str) -> Result<()> {
        let msg = json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": text }] } });
        let mut line = serde_json::to_vec(&msg)?;
        line.push(b'\n');
        let mut buf = Vec::new();
        Frame::Data(line).write_to(&mut buf)?;
        self.stdin.write_all(&buf).await?;
        Ok(())
    }
}

fn argv(run: &FirmRun, w: &WorkerRt, home: &str, resume: Option<&str>) -> Vec<String> {
    let role = &run.config.roles[&w.role];
    if role.fake.is_some() {
        return vec![super::ops::AGENT_PATH.into(), "fake-claude".into(), format!("{home}/.office/fake.yaml")];
    }
    let mut v: Vec<String> = ["claude", "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--permission-mode", "bypassPermissions"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Some(id) = resume {
        v.push("--resume".into());
        v.push(id.into());
    }
    v
}

/// What the worker is doing, from a tool call: `Edit pricing.md`.
fn activity(event: &Value) -> Option<String> {
    let content = event["message"]["content"].as_array()?;
    let tool = content.iter().rev().find(|c| c["type"] == "tool_use")?;
    let name = tool["name"].as_str().unwrap_or("tool");
    let name = name.strip_prefix("mcp__office__").unwrap_or(name);
    let input = &tool["input"];
    let arg = ["file_path", "path", "command", "pattern", "url", "subject", "query", "description"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .map(|s| {
            let s = s.lines().next().unwrap_or("");
            let s = s.rsplit('/').next().filter(|_| !s.contains(' ')).unwrap_or(s);
            s.chars().take(60).collect::<String>()
        })
        .unwrap_or_default();
    Some(format!("{name} {arg}").trim().to_string())
}

async fn start(run: &Arc<FirmRun>, w: &Arc<WorkerRt>, s: &Arc<Session>, transcript: Arc<Journal>, resume: Option<&str>, session_id: Arc<std::sync::Mutex<Option<String>>>) -> Result<Proc> {
    let home = format!("/home/{}", s.resolved.agent.user);
    let req = ShellRequest { id: 0, argv: argv(run, w, &home, resume), tty: None, env: Default::default(), system: false };
    let (stream, _) = s.open_shell(req).await?;
    let (mut r, stdin) = stream.into_split();
    let (tx, events) = mpsc::channel(16);
    let (run, w) = (run.clone(), w.clone());
    tokio::spawn(async move {
        let mut buf: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let mut last_activity = Instant::now() - Duration::from_secs(10);
        let mut code = None;
        let line_out = |line: &[u8], buf_err: bool| -> Option<Value> {
            let text = String::from_utf8_lossy(line);
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            if buf_err {
                transcript.append("stderr", json!({ "text": text.chars().take(4096).collect::<String>() }));
                return None;
            }
            match serde_json::from_str::<Value>(text) {
                Ok(v) if v.is_object() => {
                    transcript.write(v.clone());
                    Some(v)
                }
                _ => {
                    transcript.append("raw", json!({ "text": text.chars().take(4096).collect::<String>() }));
                    None
                }
            }
        };
        loop {
            let frame = match read_frame(&mut r).await {
                Ok(Some(f)) => f,
                _ => break,
            };
            let (data, is_err) = match frame {
                Frame::Data(d) => (d, false),
                Frame::Stderr(d) => (d, true),
                Frame::Exit(c) => {
                    code = Some(c);
                    break;
                }
                _ => continue,
            };
            let b = if is_err { &mut err } else { &mut buf };
            b.extend_from_slice(&data);
            while let Some(i) = b.iter().position(|c| *c == b'\n') {
                let line: Vec<u8> = b.drain(..=i).collect();
                let Some(v) = line_out(&line, is_err) else { continue };
                if let Some(id) = v["session_id"].as_str() {
                    let mut sid = session_id.lock().unwrap();
                    if sid.as_deref() != Some(id) {
                        *sid = Some(id.to_string());
                    }
                }
                match v["type"].as_str() {
                    Some("result") => {
                        let _ = tx.send(Out::Result(v)).await;
                    }
                    Some("assistant") => {
                        if last_activity.elapsed() > Duration::from_secs(3) {
                            if let Some(a) = activity(&v) {
                                last_activity = Instant::now();
                                let st = w.state();
                                if matches!(st, WorkerState::Working | WorkerState::Reviewing) {
                                    run.set_state(&w, st, Some(a));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if b.len() > MAX_LINE {
                let line: Vec<u8> = std::mem::take(b);
                transcript.append("truncated", json!({ "bytes": line.len() }));
            }
        }
        let _ = tx.send(Out::Exit(code)).await;
    });
    Ok(Proc { stdin, events })
}

/// Tokens of a turn: input, output and cache writes.
fn tokens(result: &Value) -> u64 {
    let u = &result["usage"];
    ["input_tokens", "output_tokens", "cache_creation_input_tokens"].iter().map(|k| u[*k].as_u64().unwrap_or(0)).sum()
}

/// A turn that failed before Claude could work: the API refused it (bad
/// token, overload). What it was handed must not be lost.
fn api_failure(result: &Value) -> Option<String> {
    if result["is_error"] != json!(true) {
        return None;
    }
    let text = result["result"].as_str().unwrap_or("");
    (text.contains("API Error") || text.contains("Failed to authenticate")).then(|| text.chars().take(200).collect())
}

fn rate_limited(result: &Value) -> bool {
    if result["is_error"] != json!(true) {
        return false;
    }
    let text = result["result"].as_str().unwrap_or("").to_ascii_lowercase();
    text.contains("rate limit") || text.contains("rate_limit") || text.contains("usage limit") || text.contains("429") || text.contains("overloaded")
}

fn idle_state(run: &FirmRun, w: &WorkerRt) -> WorkerState {
    if run.post.lock().unwrap().desk(&w.name).waiting.is_empty() {
        WorkerState::Idle
    } else {
        WorkerState::Waiting
    }
}

/// Waits for a reason to look again, or a timeout.
async fn nap(run: &FirmRun, w: &WorkerRt, secs: u64) {
    tokio::select! {
        _ = w.wake.notified() => {}
        _ = run.stopping.notified() => {}
        _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
    }
}

pub async fn run(_d: Arc<Daemon>, run: Arc<FirmRun>, w: Arc<WorkerRt>) {
    let transcript = match Journal::create(&run.dir.join("transcripts").join(format!("{}.jsonl", w.name))) {
        Ok(j) => Arc::new(j),
        Err(e) => {
            run.set_state(&w, WorkerState::Error, Some(format!("transcript: {e:#}")));
            return;
        }
    };
    let session_id = Arc::new(std::sync::Mutex::new(None::<String>));
    let mut proc: Option<Proc> = None;
    let mut restarts: VecDeque<f64> = VecDeque::new();
    let mut backoff_until = 0.0;
    run.set_state(&w, idle_state(&run, &w), None);
    loop {
        if run.ended() {
            return;
        }
        let Some(session) = w.session() else { return };
        let paused = *run.state.lock().unwrap() == RunState::Paused;
        let has_work = !w.queue.lock().unwrap().is_empty() || security::has_work(&run, &w);
        if paused || !has_work || now() < backoff_until {
            let st = w.state();
            if !matches!(st, WorkerState::Error) {
                run.set_state(&w, idle_state(&run, &w), None);
            }
            nap(&run, &w, 5).await;
            continue;
        }
        if !run.budget_allows(&w.name) {
            run.set_state(&w, WorkerState::OverBudget, None);
            nap(&run, &w, 30).await;
            continue;
        }
        let permit = match run.turns.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                run.set_state(&w, WorkerState::Queued, None);
                tokio::select! {
                    p = run.turns.clone().acquire_owned() => match p { Ok(p) => p, Err(_) => return },
                    _ = run.stopping.notified() => return,
                }
            }
        };
        if run.ended() {
            return;
        }
        if proc.is_none() {
            let resume = session_id.lock().unwrap().clone();
            match start(&run, &w, &session, transcript.clone(), resume.as_deref(), session_id.clone()).await {
                Ok(p) => proc = Some(p),
                Err(e) => {
                    run.set_state(&w, WorkerState::Error, Some(format!("starting Claude: {e:#}")));
                    drop(permit);
                    nap(&run, &w, 30).await;
                    continue;
                }
            }
        }
        // What this turn delivers.
        let ids: Vec<String> = std::mem::take(&mut *w.queue.lock().unwrap());
        let notes: Vec<String> = std::mem::take(&mut *w.notes.lock().unwrap());
        let access_ids: Vec<String> = w.access_queue.lock().unwrap().clone();
        let digest_was_due = w.digest_due.load(std::sync::atomic::Ordering::Relaxed);
        let officer_part = security::take_wake(&run, &w);
        let reviewing = officer_part.is_some();
        // Security first: never hold the post office's lock while taking it.
        let pending = security::pending_for(&run, &w.name);
        let prompt = {
            let mut po = run.post.lock().unwrap();
            po.mark_read(&w.name, &ids);
            let mut p = String::new();
            if let Some(o) = &officer_part {
                p.push_str(o);
                p.push('\n');
            }
            if !ids.is_empty() || !notes.is_empty() || officer_part.is_none() {
                p.push_str(&prompts::wake(&run.config, &po, &run.journal, &w.name, &ids, &notes, &pending, now()));
            }
            p
        };
        let first_thread = ids.first().and_then(|id| run.post.lock().unwrap().messages.get(id).map(|m| m.thread.clone()));
        *w.context.lock().unwrap() = first_thread;
        run.set_state(&w, if reviewing { WorkerState::Reviewing } else { WorkerState::Working }, None);
        run.journal.append("turn", json!({ "worker": w.name, "event": "start", "messages": ids, "reviewing": reviewing }));
        transcript.append("wake", json!({ "text": prompt }));
        let started = Instant::now();
        let p = proc.as_mut().unwrap();
        let outcome = match p.send(&prompt).await {
            Ok(()) => {
                tokio::select! {
                    o = p.events.recv() => o.unwrap_or(Out::Exit(None)),
                    _ = run.stopping.notified() => return,
                }
            }
            Err(_) => Out::Exit(None),
        };
        let ms = started.elapsed().as_millis() as u64;
        match outcome {
            Out::Result(r) => {
                let t = tokens(&r);
                if rate_limited(&r) {
                    // Not the worker's fault: try the same mail again later.
                    requeue(&w, ids);
                    requeue_access(&w, access_ids, digest_was_due);
                    backoff_until = now() + 60.0;
                    run.journal.append("turn", json!({ "worker": w.name, "event": "rate_limited", "ms": ms }));
                } else if let Some(why) = api_failure(&r) {
                    // Claude never saw the mail: keep it, say why, retry later.
                    requeue(&w, ids);
                    requeue_access(&w, access_ids, digest_was_due);
                    backoff_until = now() + 60.0;
                    run.journal.append("turn", json!({ "worker": w.name, "event": "failed", "ms": ms, "error": why }));
                    run.set_state(&w, WorkerState::Error, Some(why));
                    drop(permit);
                    nap(&run, &w, 60).await;
                    continue;
                } else {
                    run.count_turn(&w.name, t);
                    run.journal.append(
                        "turn",
                        json!({
                            "worker": w.name, "event": "end", "ms": ms, "tokens": t, "usage": r["usage"],
                            "num_turns": r["num_turns"], "is_error": r["is_error"],
                        }),
                    );
                }
            }
            Out::Exit(code) => {
                proc = None;
                requeue(&w, ids);
                let at = now();
                w.notes.lock().unwrap().push(prompts::restart_note(at));
                run.journal.append("turn", json!({ "worker": w.name, "event": "interrupted", "ms": ms, "exit_code": code }));
                restarts.push_back(at);
                while restarts.front().is_some_and(|t| at - t > 600.0) {
                    restarts.pop_front();
                }
                if restarts.len() > MAX_RESTARTS {
                    run.set_state(&w, WorkerState::Error, Some(format!("Claude exited {} times in ten minutes (last code {code:?})", restarts.len())));
                    drop(permit);
                    nap(&run, &w, 120).await;
                    restarts.clear();
                    continue;
                }
            }
        }
        drop(permit);
        *w.context.lock().unwrap() = None;
    }
}

/// Puts an officer's undecided requests back, and the digest if it was due.
fn requeue_access(w: &WorkerRt, ids: Vec<String>, digest: bool) {
    if ids.is_empty() && !digest {
        return;
    }
    let mut q = w.access_queue.lock().unwrap();
    let rest = std::mem::take(&mut *q);
    *q = ids.into_iter().chain(rest).collect();
    if digest {
        w.digest_due.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Puts undelivered mail back at the front of the queue.
fn requeue(w: &WorkerRt, ids: Vec<String>) {
    let mut q = w.queue.lock().unwrap();
    let rest = std::mem::take(&mut *q);
    *q = ids.into_iter().chain(rest).collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activities() {
        let e = json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Edit", "input": { "file_path": "/work/pricing.md" } }] } });
        assert_eq!(activity(&e).unwrap(), "Edit pricing.md");
        let e = json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "mcp__office__mail_send", "input": { "subject": "Plan" } }] } });
        assert_eq!(activity(&e).unwrap(), "mail_send Plan");
        let e = json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "hi" }] } });
        assert!(activity(&e).is_none());
    }

    #[test]
    fn api_failures() {
        assert!(api_failure(&json!({ "is_error": true, "result": "Failed to authenticate. API Error: 401 Invalid bearer token" })).is_some());
        assert!(api_failure(&json!({ "is_error": true, "result": "reached max turns" })).is_none());
        assert!(api_failure(&json!({ "is_error": false, "result": "API Error mentioned in prose" })).is_none());
    }

    #[test]
    fn rate_limits() {
        assert!(rate_limited(&json!({ "is_error": true, "result": "API Error: 429 rate_limit_error" })));
        assert!(!rate_limited(&json!({ "is_error": false, "result": "rate limit" })));
    }
}
