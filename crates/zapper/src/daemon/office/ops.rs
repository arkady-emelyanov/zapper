//! The office protocol: a worker's VM connects to the host's office port and
//! sends one JSON request, `{"op": "...", "args": {...}}`, and gets one JSON
//! response, `{"ok": ...}` or `{"error": "..."}`. The daemon knows which
//! worker it is from the VM the connection came from; it checks the role on
//! every request. `zapper-agent office` turns the ops into MCP tools.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use zapper_policy::proto::MAX_OFFICE_LINE;
use zapper_policy::{parse_duration, NetRequest, BOARD};

use super::mail::{Draft, Expects, FollowUp, Kind, PostOffice};
use super::{drive, now, security, FirmLink, FirmRun};
use crate::daemon::session::Session;
use crate::daemon::Daemon;

/// Where the agent copies itself in every guest.
pub const AGENT_PATH: &str = "/usr/local/bin/zapper-agent";

pub async fn serve(d: Arc<Daemon>, s: Arc<Session>, link: FirmLink, listener: UnixListener) {
    let limit = Arc::new(Semaphore::new(8));
    loop {
        let Ok((stream, _)) = listener.accept().await else { break };
        let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
        let (d, s, link) = (d.clone(), s.clone(), link.clone());
        tokio::spawn(async move {
            let _ = handle(d, s, link, stream).await;
            drop(permit);
        });
    }
}

async fn handle(d: Arc<Daemon>, s: Arc<Session>, link: FirmLink, stream: UnixStream) -> Result<()> {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = Vec::new();
    let n = tokio::time::timeout(Duration::from_secs(30), (&mut reader).take(MAX_OFFICE_LINE as u64 + 1).read_until(b'\n', &mut line)).await??;
    let reply = if n > MAX_OFFICE_LINE {
        json!({ "error": format!("request longer than {MAX_OFFICE_LINE} bytes") })
    } else {
        let run = link.run();
        match (run, serde_json::from_slice::<Value>(&line)) {
            (None, _) => json!({ "error": "the firm isn't running" }),
            (_, Err(e)) => json!({ "error": format!("bad request: {e}") }),
            (Some(run), Ok(req)) => {
                let op = req["op"].as_str().unwrap_or("").to_string();
                let args = req.get("args").cloned().unwrap_or(json!({}));
                match dispatch(&d, &run, &s, &link.member, &op, &args).await {
                    Ok(v) => json!({ "ok": v }),
                    Err(e) => {
                        run.journal.append("op_error", json!({ "worker": link.member, "op": op, "error": format!("{e:#}") }));
                        json!({ "error": format!("{e:#}") })
                    }
                }
            }
        }
    };
    let mut out = serde_json::to_vec(&reply)?;
    out.push(b'\n');
    w.write_all(&out).await?;
    Ok(())
}

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": props, "required": required } })
}

fn names() -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": "Addresses: a worker's name, board, team:NAME (NAME's direct reports), or all" })
}

/// The tools a worker's MCP server offers: by role.
pub fn tools(officer: bool) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let mut v = vec![
        tool(
            "mail_send",
            "Start a new mail thread. Use kind=assignment to give work to your reports (only managers and the board may), question to ask, fyi to inform. Returns the message and thread ids. Don't wait for an answer: end your turn; the reply wakes you.",
            json!({
                "to": names(), "cc": names(), "subject": s("States the ask"), "body": s("Markdown; start with the ask, then context, then what done looks like"),
                "kind": { "type": "string", "enum": ["assignment", "question", "report", "fyi", "reply"] },
                "expects": { "type": "string", "enum": ["reply", "done", "nothing"], "description": "Default: assignment→done, question→reply, else nothing" },
                "due": s("When the answer is due, relative: 2h, 1d, 90m"),
                "attachments": { "type": "array", "items": { "type": "string" }, "description": "Shared drive references like drive:a-3@v2" },
                "follow_up": { "type": "object", "properties": { "every": { "type": "string" }, "max": { "type": "integer" }, "off": { "type": "boolean" } } },
            }),
            &["to", "subject", "body", "kind"],
        ),
        tool(
            "mail_reply",
            "Reply in a message's thread, to its sender (all=true: everyone in the thread). For an assignment you finished: kind=report and close=true, with a summary and drive attachments.",
            json!({
                "message": s("Message id, e.g. m-12"), "body": s("Markdown"),
                "kind": { "type": "string", "enum": ["reply", "report", "question", "fyi"] },
                "attachments": { "type": "array", "items": { "type": "string" } },
                "close": { "type": "boolean", "description": "Marks the assignment you were given in this thread as done" },
                "all": { "type": "boolean" },
            }),
            &["message", "body"],
        ),
        tool("mail_inbox", "List messages to you, newest first.", json!({ "unread": { "type": "boolean" } }), &[]),
        tool("mail_thread", "A whole thread, oldest first, with bodies.", json!({ "thread": s("Thread id, e.g. t-12") }), &["thread"]),
        tool("mail_read", "One message with its body.", json!({ "message": s("Message id") }), &["message"]),
        tool("desk", "What you owe (assignments and questions to you), what you wait for from others, and your pending access requests.", json!({}), &[]),
        tool("directory", "The org chart: names, titles, roles, managers, reports, security officers, and who is busy.", json!({}), &[]),
        tool(
            "request_access",
            "Ask the security officers for network access before you need it, with a reason. You'll get mail with the decision.",
            json!({ "host": s("Host name"), "port": { "type": "integer" }, "methods": { "type": "array", "items": { "type": "string" } }, "path": s("URL path glob, e.g. /simple/*"), "reason": s("Why your work needs it") }),
            &["host", "reason"],
        ),
        tool(
            "drive_publish",
            "Publish files from your VM to the shared drive (paths under /work, your home or /tmp). Give artifact=a-N to add a new version of your own artifact, with `changes` saying what changed.",
            json!({
                "paths": { "type": "array", "items": { "type": "string" } }, "title": s("Title"),
                "kind": { "type": "string", "enum": ["document", "report", "dataset", "deck", "board-pack"] },
                "summary": s("One paragraph: what it is and what it says"),
                "audience": { "description": "firm (default), board, or a list of names" },
                "artifact": s("Existing artifact id to add a version to"), "thread": s("The thread it answers"),
                "changes": s("What changed from the previous version, for readers: one or two lines. Required with artifact; optional for a first version"),
            }),
            &["paths", "title", "kind", "summary"],
        ),
        tool("drive_list", "Find artifacts on the shared drive.", json!({ "kind": s("Kind"), "author": s("Author"), "query": s("Words in the title or summary") }), &[]),
        tool("drive_fetch", "Copy an artifact into your VM.", json!({ "artifact": s("a-3 or a-3@v2 or drive:a-3@v2"), "dest": s("Directory, e.g. /work/in") }), &["artifact", "dest"]),
    ];
    if officer {
        v.extend([
            tool("traffic", "The firm's requests: method, URL, status, sizes, rule, allowed or denied.", json!({ "worker": s("Worker"), "host": s("Host prefix"), "since": s("e.g. 1h"), "limit": { "type": "integer" } }), &[]),
            tool("traffic_digest", "A summary of traffic since the last digest: new hosts, large uploads, approvals.", json!({}), &[]),
            tool("access_requests", "Pending access requests.", json!({}), &[]),
            tool("access_rules", "Standing approvals made so far, with how many requests each allowed.", json!({}), &[]),
            tool(
                "access_decide",
                "Decide an access request. scope once (one connection or request), worker or role (standing). Narrow with methods and path; give an expiry.",
                json!({
                    "request": s("r-4"), "allow": { "type": "boolean" }, "scope": { "type": "string", "enum": ["once", "worker", "role"] }, "reason": s("One line; the worker and the board read it"),
                    "methods": { "type": "array", "items": { "type": "string" } }, "path": s("URL path glob"), "expires_in": s("e.g. 4h"),
                }),
                &["request", "allow", "scope", "reason"],
            ),
            tool("access_revoke", "Remove a standing approval.", json!({ "rule": s("Rule id"), "reason": s("Why") }), &["rule", "reason"]),
            tool("escalate", "Hand a request to the board: secrets, private networks, the firm's own services, global rules.", json!({ "request": s("r-4"), "recommendation": s("What you recommend and why") }), &["request", "recommendation"]),
        ]);
    }
    v
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => vec![],
    }
}

fn opt_str(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(str::to_string).filter(|s| !s.is_empty())
}

fn req_str(v: &Value, k: &str) -> Result<String> {
    opt_str(v, k).ok_or_else(|| anyhow!("{k} is required"))
}

fn parse_kind(s: &str) -> Result<Kind> {
    serde_json::from_value(json!(s)).map_err(|_| anyhow!("kind {s:?}: assignment, question, reply, report or fyi"))
}

fn parse_due(v: &Value) -> Result<Option<f64>> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(n.as_f64()),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => {
            let s = s.trim().trim_start_matches("in ").trim();
            Ok(Some(now() + parse_duration(s).map_err(|_| anyhow!("due {s:?}: give a relative time like 2h, 1d or 90m"))?.as_secs_f64()))
        }
        _ => bail!("due: give a relative time like 2h"),
    }
}

fn attachments(v: &Value) -> Result<Vec<String>> {
    let list = strings(v);
    for a in &list {
        let id = a.trim_start_matches("drive:");
        let ok = id.starts_with("a-") && id[2..].split('@').next().is_some_and(|n| n.parse::<u64>().is_ok());
        if !ok {
            bail!("attachment {a:?}: give a shared drive reference like drive:a-3@v2");
        }
    }
    Ok(list.into_iter().map(|a| if a.starts_with("drive:") { a } else { format!("drive:{a}") }).collect())
}

/// A draft from `mail_send` arguments (also the board's API).
pub fn draft_from(from: &str, v: &Value, context: Option<String>, po: &PostOffice) -> Result<Draft> {
    let kind = parse_kind(&opt_str(v, "kind").unwrap_or_else(|| "fyi".into()))?;
    let mut d = Draft::new(from, strings(&v["to"]), &opt_str(v, "subject").unwrap_or_default(), &opt_str(v, "body").unwrap_or_default(), kind);
    d.cc = strings(&v["cc"]);
    d.expects = match opt_str(v, "expects") {
        Some(e) => Some(serde_json::from_value::<Expects>(json!(e)).map_err(|_| anyhow!("expects: reply, done or nothing"))?),
        None => None,
    };
    d.due = parse_due(&v["due"])?;
    d.attachments = attachments(&v["attachments"])?;
    d.context = context;
    if let Some(r) = opt_str(v, "in_reply_to") {
        if !po.messages.contains_key(&r) {
            bail!("no message {r}");
        }
        d.in_reply_to = Some(r);
    }
    d.close = v["close"].as_bool().unwrap_or(false);
    let f = &v["follow_up"];
    d.follow_up = FollowUp {
        every: match f["every"].as_str() {
            Some(e) => Some(parse_duration(e)?.as_secs_f64()),
            None => f["every"].as_f64(),
        },
        max: f["max"].as_u64().map(|m| m.min(10) as u32),
        off: f["off"].as_bool().unwrap_or(false),
    };
    if d.body.trim().is_empty() {
        bail!("body is required");
    }
    Ok(d)
}

/// A reply to `message` from `from`.
pub fn reply_from(from: &str, v: &Value, po: &PostOffice) -> Result<Draft> {
    let id = req_str(v, "message")?;
    let m = po.messages.get(&id).ok_or_else(|| anyhow!("no message {id}"))?;
    if !po.can_read(from, &m.thread) {
        bail!("no message {id}");
    }
    let close = v["close"].as_bool().unwrap_or(false);
    let kind = match opt_str(v, "kind") {
        Some(k) => parse_kind(&k)?,
        None if close => Kind::Report,
        None => Kind::Reply,
    };
    if kind == Kind::Assignment {
        bail!("an assignment starts a thread: use mail_send");
    }
    let to: Vec<String> = if v["all"].as_bool().unwrap_or(false) {
        po.threads.get(&m.thread).map(|t| t.participants.iter().filter(|p| *p != from && *p != super::mail::OFFICE).cloned().collect()).unwrap_or_default()
    } else if m.from == from {
        m.to.clone()
    } else if m.from == super::mail::OFFICE {
        bail!("{id} is from the post office; reply to the original message instead")
    } else {
        vec![m.from.clone()]
    };
    let mut d = Draft::new(from, to, "", &req_str(v, "body")?, kind);
    d.in_reply_to = Some(id);
    d.close = close;
    d.attachments = attachments(&v["attachments"])?;
    Ok(d)
}

pub async fn dispatch(_d: &Arc<Daemon>, run: &Arc<FirmRun>, s: &Arc<Session>, me: &str, op: &str, args: &Value) -> Result<Value> {
    let w = run.worker(me)?.clone();
    let officer_only = ["traffic", "traffic_digest", "access_requests", "access_rules", "access_decide", "access_revoke", "escalate"];
    if officer_only.contains(&op) && !w.officer {
        bail!("{op} is for security officers");
    }
    if run.ended() {
        bail!("the firm is stopping");
    }
    match op {
        "tools" => Ok(json!(tools(w.officer))),
        "mail_send" => {
            let context = w.context.lock().unwrap().clone();
            let d = draft_from(me, args, context, &run.post.lock().unwrap())?;
            if d.in_reply_to.is_some() {
                bail!("to answer a message use mail_reply");
            }
            check_attachments(run, me, &d.attachments)?;
            let sent = run.send(d)?;
            Ok(json!(sent))
        }
        "mail_reply" => {
            let d = reply_from(me, args, &run.post.lock().unwrap())?;
            check_attachments(run, me, &d.attachments)?;
            Ok(json!(run.send(d)?))
        }
        "mail_inbox" => {
            let unread = args["unread"].as_bool().unwrap_or(false);
            Ok(json!(run.post.lock().unwrap().inbox(me, unread, 50)))
        }
        "mail_thread" => {
            let id = req_str(args, "thread")?;
            let mut po = run.post.lock().unwrap();
            let v = po.thread(&run.journal, &id, me)?;
            let ids: Vec<String> = v["messages"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
            po.mark_read(me, &ids);
            Ok(v)
        }
        "mail_read" => {
            let id = req_str(args, "message")?;
            let mut po = run.post.lock().unwrap();
            let v = po.message(&run.journal, &id, me)?;
            po.mark_read(me, &[id]);
            Ok(v)
        }
        "desk" => {
            let desk = run.post.lock().unwrap().desk(me);
            Ok(json!({ "owed": desk.owed, "waiting": desk.waiting, "access_requests": security::pending_for(run, me) }))
        }
        "directory" => {
            let mut v = run.post.lock().unwrap().directory();
            for x in v["workers"].as_array_mut().into_iter().flatten() {
                if let Some(rt) = x["name"].as_str().and_then(|n| run.workers.get(n)) {
                    x["state"] = json!(rt.state());
                }
            }
            Ok(v)
        }
        "request_access" => {
            let host = zapper_policy::names::hostname(&req_str(args, "host")?).map_err(|e| anyhow!("host: {e}"))?;
            let port = args["port"].as_u64().unwrap_or(443).min(65535) as u16;
            let methods = args["methods"].as_array().map(|_| strings(&args["methods"]).into_iter().map(|m| m.to_ascii_uppercase()).collect::<Vec<_>>());
            let path = opt_str(args, "path");
            let reason = req_str(args, "reason")?;
            let req = NetRequest { host, port, method: methods.as_ref().and_then(|m| m.first().cloned()), path: None };
            let id = security::ask(run, me, &req, methods, path, &reason)?;
            Ok(json!({ "request": id, "note": "the security officers will decide; you'll get mail" }))
        }
        "drive_publish" => {
            let a = drive::PublishArgs {
                paths: strings(&args["paths"]),
                title: req_str(args, "title")?,
                kind: req_str(args, "kind")?,
                summary: opt_str(args, "summary").unwrap_or_default(),
                audience: args["audience"].clone(),
                artifact: opt_str(args, "artifact"),
                thread: opt_str(args, "thread").or_else(|| w.context.lock().unwrap().clone()),
                changes: opt_str(args, "changes").unwrap_or_default(),
            };
            drive::publish(run, me, s, a).await
        }
        "drive_list" => {
            let d = run.drive.lock().unwrap();
            Ok(json!(d.list(me, opt_str(args, "kind").as_deref(), opt_str(args, "author").as_deref(), opt_str(args, "query").as_deref())))
        }
        "drive_fetch" => drive::fetch(run, me, s, &req_str(args, "artifact")?, &req_str(args, "dest")?).await,
        "traffic" => {
            let since = match opt_str(args, "since") {
                Some(x) => Some(now() - parse_duration(&x)?.as_secs_f64()),
                None => None,
            };
            let limit = args["limit"].as_u64().unwrap_or(100).min(500) as usize;
            Ok(json!(security::traffic(run, opt_str(args, "worker").as_deref(), opt_str(args, "host").as_deref(), since, limit)))
        }
        "traffic_digest" => {
            w.digest_due.store(false, std::sync::atomic::Ordering::Relaxed);
            Ok(json!(security::take_wake_digest(run).unwrap_or_else(|| "No new traffic since the last digest.".into())))
        }
        "access_requests" => Ok(json!(security::requests(run, true))),
        "access_rules" => Ok(json!(security::standing(run))),
        "access_decide" => {
            let a = security::DecideArgs {
                allow: args["allow"].as_bool().ok_or_else(|| anyhow!("allow is required"))?,
                scope: req_str(args, "scope")?,
                reason: req_str(args, "reason")?,
                methods: args["methods"].as_array().map(|_| strings(&args["methods"])),
                path: opt_str(args, "path"),
                expires_in: opt_str(args, "expires_in"),
            };
            security::decide(run, me, &req_str(args, "request")?, a)
        }
        "access_revoke" => security::revoke(run, me, &req_str(args, "rule")?, &req_str(args, "reason")?),
        "escalate" => security::escalate(run, me, &req_str(args, "request")?, &req_str(args, "recommendation")?),
        _ => bail!("unknown op {op:?}; ops: tools, {}", tools(w.officer).iter().filter_map(|t| t["name"].as_str()).collect::<Vec<_>>().join(", ")),
    }
}

fn check_attachments(run: &FirmRun, me: &str, list: &[String]) -> Result<()> {
    let d = run.drive.lock().unwrap();
    for a in list {
        d.resolve(a, me)?;
    }
    Ok(())
}

#[allow(dead_code)]
fn board() -> &'static str {
    BOARD
}
