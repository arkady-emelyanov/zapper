//! `zapper firm`, `zapper mail` and `zapper drive`: the office from the
//! command line, as the board.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use http_body_util::BodyExt;
use hyper::Method;
use serde_json::{json, Value};

use crate::client::{url_encode, Client};
use crate::{print_json, s, table};

#[derive(Subcommand)]
pub enum FirmCmd {
    /// List firms and their runs' states.
    Ls,
    /// Start a fresh run: boot the facilities, then every worker, idle.
    Up { firm: String },
    /// Stop the run; its journal, transcripts and shared drive stay on disk.
    Down { firm: String },
    /// Workers' states, budgets and the run.
    Status { firm: String },
    /// Stop starting turns; workers finish the turn they are in.
    Pause { firm: String },
    Resume { firm: String },
    /// Past runs, newest first.
    Runs { firm: String },
    /// Delete a past run's directory.
    Prune { firm: String, run: String },
    /// The run's journal (mail, states, turns, access, publishes).
    Journal {
        firm: String,
        #[arg(long, default_value = "current")]
        run: String,
        #[arg(short, long)]
        follow: bool,
    },
    /// A worker's Claude transcript.
    Transcript {
        firm: String,
        worker: String,
        #[arg(long, default_value = "current")]
        run: String,
        #[arg(short, long)]
        follow: bool,
    },
    /// Print the UI address that replays a run.
    Replay { firm: String, run: String },
    /// Access requests and standing approvals.
    Security { firm: String },
    /// Decide an access request as the board.
    Decide {
        firm: String,
        request: String,
        #[arg(long, conflicts_with = "deny")]
        allow: bool,
        #[arg(long)]
        deny: bool,
        /// once, worker or role.
        #[arg(long, default_value = "once")]
        scope: String,
        #[arg(long, default_value = "decided by the board")]
        reason: String,
    },
    /// Remove a standing approval.
    Revoke { firm: String, rule: String },
}

#[derive(Subcommand)]
pub enum MailCmd {
    /// Write as the board: `zapper mail send acme --to ceo --subject … --kind assignment < brief.md`
    Send {
        firm: String,
        #[arg(long, value_delimiter = ',', required = true)]
        to: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        cc: Vec<String>,
        #[arg(long, default_value = "")]
        subject: String,
        /// The body; read from stdin if not given.
        #[arg(long)]
        body: Option<String>,
        /// assignment, question, fyi, reply or report.
        #[arg(long, default_value = "assignment")]
        kind: String,
        /// Relative, e.g. 2h or 1d.
        #[arg(long)]
        due: Option<String>,
        /// Answer this message, in its thread.
        #[arg(long)]
        reply_to: Option<String>,
    },
    /// The board's inbox (or a worker's with --for).
    Ls {
        firm: String,
        #[arg(long = "for")]
        who: Option<String>,
        #[arg(long)]
        unread: bool,
    },
    /// A thread with its messages.
    Thread { firm: String, thread: String },
}

#[derive(Subcommand)]
pub enum DriveCmd {
    /// Artifacts of the current run.
    Ls { firm: String },
    /// Download an artifact's files: `zapper drive get acme a-3@v2 ./out`
    Get {
        firm: String,
        artifact: String,
        dir: PathBuf,
        #[arg(long, default_value = "current")]
        run: String,
    },
}

fn time(ts: &Value) -> String {
    let t = ts.as_f64().unwrap_or(0.0) as i64;
    format!("{:02}:{:02}:{:02}", (t / 3600) % 24, (t / 60) % 60, t % 60)
}

/// One journal entry as a line of text.
pub fn journal_line(e: &Value) -> String {
    let t = time(&e["ts"]);
    let what = match e["type"].as_str().unwrap_or("") {
        "mail" => format!(
            "mail {} {} → {} [{}] \"{}\"{}",
            s(&e["id"]),
            s(&e["from"]),
            e["to"].as_array().map(|a| a.iter().map(s).collect::<Vec<_>>().join(",")).unwrap_or_default(),
            s(&e["kind"]),
            s(&e["subject"]),
            if e["automatic"] == json!(true) { " (automatic)" } else { "" }
        ),
        "worker" => format!("{} is {}{}", s(&e["worker"]), s(&e["state"]), e["detail"].as_str().map(|d| format!(": {d}")).unwrap_or_default()),
        "turn" => format!("turn {} {}{}", s(&e["worker"]), s(&e["event"]), e["ms"].as_u64().map(|m| format!(" {:.1}s", m as f64 / 1000.0)).unwrap_or_default()),
        "access" => {
            let r = &e["request"];
            format!("access {} {} {} {}:{}", s(&e["event"]), s(&r["id"]), s(&r["worker"]), s(&r["host"]), r["port"])
        }
        "published" => format!("published {}@v{} \"{}\" by {}", s(&e["artifact"]), e["version"], s(&e["title"]), s(&e["author"])),
        "run" => format!("run {}{}", s(&e["state"]), e["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default()),
        other => format!("{other} {}", e),
    };
    format!("{t} {what}")
}

async fn follow(c: &Client, path: &str, render: impl Fn(&Value) -> String, json_out: bool) -> Result<()> {
    let mut body = c.stream(path).await?;
    let mut buf = String::new();
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        let Some(data) = frame.data_ref() else { continue };
        buf.push_str(&String::from_utf8_lossy(data));
        while let Some(i) = buf.find('\n') {
            let line: String = buf.drain(..=i).collect();
            if let Some(j) = line.trim().strip_prefix("data:") {
                if json_out {
                    println!("{}", j.trim());
                } else if let Ok(v) = serde_json::from_str::<Value>(j.trim()) {
                    println!("{}", render(&v));
                }
            }
        }
    }
    Ok(())
}

fn transcript_line(e: &Value) -> String {
    let t = time(&e["ts"]);
    match e["type"].as_str().unwrap_or("") {
        "wake" => format!("{t} ── wake ──\n{}", s(&e["text"])),
        "assistant" => {
            let parts: Vec<String> = e["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| match c["type"].as_str() {
                    Some("text") => s(&c["text"]),
                    Some("tool_use") => format!("→ {} {}", s(&c["name"]), c["input"]),
                    Some("thinking") => "(thinking)".into(),
                    _ => String::new(),
                })
                .filter(|x| !x.is_empty())
                .collect();
            format!("{t} {}", parts.join("\n"))
        }
        "user" => {
            let n = e["message"]["content"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("{t} ({n} tool result(s))")
        }
        "result" => format!("{t} ── turn ended: {} ──", s(&e["result"]).chars().take(200).collect::<String>()),
        "system" => format!("{t} ({})", s(&e["subtype"])),
        other => format!("{t} {other} {}", e["text"].as_str().unwrap_or("")),
    }
}

pub async fn firm(c: &Client, cmd: FirmCmd, json_out: bool) -> Result<i32> {
    match cmd {
        FirmCmd::Ls => {
            let v = c.json(Method::GET, "/v1/firms", None).await?;
            if json_out {
                print_json(&v);
            } else {
                let rows = v.as_array().unwrap().iter().map(|f| vec![s(&f["name"]), s(&f["workers"]), s(&f["state"]), s(&f["run"]), s(&f["description"])]).collect();
                table(&["firm", "workers", "state", "run", "description"], rows);
            }
        }
        FirmCmd::Up { firm } => {
            eprintln!("zapper: starting firm {firm}…");
            let v = c.json(Method::POST, &format!("/v1/firms/{firm}/up"), None).await?;
            eprintln!("zapper: firm {firm} is running (run {}); write to the CEO with `zapper mail send {firm} --to …` or in the UI", s(&v["run"]));
            if json_out {
                print_json(&v);
            }
        }
        FirmCmd::Down { firm } => {
            let v = c.json(Method::POST, &format!("/v1/firms/{firm}/down"), None).await?;
            println!("firm {firm} stopped; run {} is in {}", s(&v["run"]), s(&v["dir"]));
        }
        FirmCmd::Pause { firm } => print_json(&c.json(Method::POST, &format!("/v1/firms/{firm}/pause"), Some(r#"{"paused": true}"#.into())).await?),
        FirmCmd::Resume { firm } => print_json(&c.json(Method::POST, &format!("/v1/firms/{firm}/pause"), Some(r#"{"paused": false}"#.into())).await?),
        FirmCmd::Status { firm } => {
            let v = c.json(Method::GET, &format!("/v1/firms/{firm}"), None).await?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let run = &v["run"];
            if run.is_null() {
                println!("firm {firm}: not started");
                return Ok(0);
            }
            println!("firm {firm}: run {} {}; turns today {} of {}, tokens {}", s(&run["run"]), s(&run["state"]), run["usage"]["turns"], run["budget"]["daily_turns"], run["usage"]["tokens"]);
            let rows = run["workers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| vec![s(&w["name"]), s(&w["title"]), s(&w["state"]), s(&w["detail"]), s(&w["unread"]), format!("{}/{}", w["owed"], w["waiting"]), s(&w["session"])])
                .collect();
            table(&["worker", "title", "state", "doing", "unread", "owes/waits", "session"], rows);
        }
        FirmCmd::Runs { firm } => {
            let v = c.json(Method::GET, &format!("/v1/firms/{firm}/runs"), None).await?;
            if json_out {
                print_json(&v);
            } else {
                let rows = v.as_array().unwrap().iter().map(|r| vec![s(&r["run"]), s(&r["state"]), format!("{} KiB", r["journal_bytes"].as_u64().unwrap_or(0) / 1024)]).collect();
                table(&["run", "state", "journal"], rows);
            }
        }
        FirmCmd::Prune { firm, run } => print_json(&c.json(Method::DELETE, &format!("/v1/firms/{firm}/runs/{run}"), None).await?),
        FirmCmd::Journal { firm, run, follow: f } => {
            let path = format!("/v1/firms/{firm}/runs/{run}/journal");
            if f {
                follow(c, &format!("{path}?follow=true"), journal_line, json_out).await?;
            } else {
                let text = c.text(&path).await?;
                for l in text.lines() {
                    match (json_out, serde_json::from_str::<Value>(l)) {
                        (false, Ok(v)) => println!("{}", journal_line(&v)),
                        _ => println!("{l}"),
                    }
                }
            }
        }
        FirmCmd::Transcript { firm, worker, run, follow: f } => {
            let path = format!("/v1/firms/{firm}/runs/{run}/transcripts/{worker}");
            if f {
                follow(c, &format!("{path}?follow=true"), transcript_line, json_out).await?;
            } else {
                for l in c.text(&path).await?.lines() {
                    match (json_out, serde_json::from_str::<Value>(l)) {
                        (false, Ok(v)) => println!("{}", transcript_line(&v)),
                        _ => println!("{l}"),
                    }
                }
            }
        }
        FirmCmd::Replay { firm, run } => {
            c.json(Method::GET, &format!("/v1/firms/{firm}/runs"), None).await?;
            let state = crate::daemon_state_dir()?;
            let url = std::fs::read_to_string(state.join("ui-url")).context("the daemon's UI address (is the UI enabled?)")?;
            println!("{}#firm={firm}&replay={run}", url.trim());
        }
        FirmCmd::Security { firm } => {
            let v = c.json(Method::GET, &format!("/v1/firms/{firm}/security"), None).await?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let rows = v["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| vec![s(&r["id"]), s(&r["worker"]), format!("{} {}:{}", r["method"].as_str().unwrap_or("CONNECT"), s(&r["host"]), r["port"]), s(&r["state"]), s(&r["officer"]), s(&r["decision"]["reason"])])
                .collect();
            table(&["request", "worker", "target", "state", "officer", "reason"], rows);
            println!();
            let rows = v["standing"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| vec![s(&r["id"]), s(&r["scope"]), format!("{} {}:{}", s(&r["rule"]["action"]), s(&r["rule"]["host"]), r["rule"]["ports"]), s(&r["by"]), s(&r["uses"]), s(&r["reason"])])
                .collect();
            table(&["rule", "scope", "rule", "by", "uses", "reason"], rows);
        }
        FirmCmd::Decide { firm, request, allow, deny, scope, reason } => {
            if allow == deny {
                bail!("give --allow or --deny");
            }
            let body = json!({ "allow": allow, "scope": scope, "reason": reason });
            print_json(&c.json(Method::POST, &format!("/v1/firms/{firm}/security/requests/{request}"), Some(body.to_string())).await?);
        }
        FirmCmd::Revoke { firm, rule } => print_json(&c.json(Method::DELETE, &format!("/v1/firms/{firm}/security/rules/{rule}"), None).await?),
    }
    Ok(0)
}

pub async fn mail(c: &Client, cmd: MailCmd, json_out: bool) -> Result<i32> {
    match cmd {
        MailCmd::Send { firm, to, cc, subject, body, kind, due, reply_to } => {
            let body = match body {
                Some(b) => b,
                None => {
                    use std::io::Read;
                    eprintln!("zapper: reading the body from stdin (end with Ctrl-D)…");
                    let mut b = String::new();
                    std::io::stdin().read_to_string(&mut b)?;
                    b
                }
            };
            let v = json!({ "to": to, "cc": cc, "subject": subject, "body": body, "kind": kind, "due": due, "in_reply_to": reply_to });
            let sent = c.json(Method::POST, &format!("/v1/firms/{firm}/mail"), Some(v.to_string())).await?;
            println!("sent {} in thread {}", s(&sent["id"]), s(&sent["thread"]));
        }
        MailCmd::Ls { firm, who, unread } => {
            let mut path = format!("/v1/firms/{firm}/mail?unread={unread}");
            if let Some(w) = who {
                path.push_str(&format!("&for={}", url_encode(&w)));
            }
            let v = c.json(Method::GET, &path, None).await?;
            if json_out {
                print_json(&v);
            } else {
                let rows = v
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| vec![s(&m["id"]), s(&m["thread"]), s(&m["from"]), s(&m["kind"]), if m["unread"] == json!(true) { "•".into() } else { String::new() }, s(&m["subject"]), s(&m["preview"])])
                    .collect();
                table(&["id", "thread", "from", "kind", "new", "subject", "preview"], rows);
            }
        }
        MailCmd::Thread { firm, thread } => {
            let v = c.json(Method::GET, &format!("/v1/firms/{firm}/threads/{thread}"), None).await?;
            if json_out {
                print_json(&v);
            } else {
                println!("{} — {}\n", s(&v["thread"]["subject"]), s(&v["thread"]["id"]));
                for m in v["messages"].as_array().into_iter().flatten() {
                    println!(
                        "── {} {} → {} [{}]{}\n{}\n",
                        s(&m["id"]),
                        s(&m["from"]),
                        m["to"].as_array().map(|a| a.iter().map(s).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                        s(&m["kind"]),
                        if m["automatic"] == json!(true) { " automatic" } else { "" },
                        s(&m["body"])
                    );
                }
            }
        }
    }
    Ok(0)
}

pub async fn drive(c: &Client, cmd: DriveCmd, json_out: bool) -> Result<i32> {
    match cmd {
        DriveCmd::Ls { firm } => {
            let v = c.json(Method::GET, &format!("/v1/firms/{firm}/drive"), None).await?;
            if json_out {
                print_json(&v);
            } else {
                let rows = v.as_array().unwrap().iter().map(|a| vec![format!("{}@v{}", s(&a["id"]), a["latest"]), s(&a["kind"]), s(&a["author"]), s(&a["title"]), s(&a["files"])]).collect();
                table(&["artifact", "kind", "author", "title", "files"], rows);
            }
        }
        DriveCmd::Get { firm, artifact, dir, run } => {
            let journal = c.text(&format!("/v1/firms/{firm}/runs/{run}/journal")).await?;
            let r = artifact.trim_start_matches("drive:");
            let (id, ver) = match r.split_once('@') {
                Some((i, v)) => (i.to_string(), Some(v.trim_start_matches('v').parse::<u64>()?)),
                None => (r.to_string(), None),
            };
            let published: Vec<Value> = journal.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|e| e["type"] == "published" && e["artifact"] == json!(id)).collect();
            let entry = match ver {
                Some(v) => published.iter().find(|e| e["version"] == json!(v)),
                None => published.last(),
            };
            let Some(entry) = entry else { bail!("no artifact {artifact} in run {run}") };
            let version = entry["version"].as_u64().unwrap_or(1);
            std::fs::create_dir_all(&dir)?;
            for f in entry["files"].as_array().into_iter().flatten() {
                let path = s(&f["path"]);
                if path.split('/').any(|p| p == ".." || p.is_empty()) {
                    continue;
                }
                let url = format!("/v1/firms/{firm}/runs/{run}/drive/{id}/{version}/{}", path.split('/').map(url_encode).collect::<Vec<_>>().join("/"));
                let (status, bytes) = c.call(Method::GET, &url, None).await?;
                if !status.is_success() {
                    bail!("{path}: {status}");
                }
                let to = dir.join(&path);
                std::fs::create_dir_all(to.parent().unwrap())?;
                std::fs::write(&to, &bytes)?;
            }
            println!("{id}@v{version} \"{}\" → {}", s(&entry["title"]), dir.display());
            if json_out {
                print_json(entry);
            }
        }
    }
    Ok(0)
}
