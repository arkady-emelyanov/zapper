//! REST routes for firms, under `/v1/firms` on the API socket and `/api/firms`
//! in the UI. Reads serve both; of the writes, the UI gets the board's own:
//! mail, firm up/down/pause, and security decisions.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use zapper_policy::{AnyRule, BOARD};

use super::{drive, security};
use crate::daemon::api::ApiError;
use crate::daemon::Daemon;

type D = State<Arc<Daemon>>;
type R<T> = Result<T, ApiError>;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn body(text: &str) -> R<Value> {
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_yaml::from_str(text).map_err(|e| bad(format!("body: {e}")))
}

pub fn read_routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/firms", get(list))
        .route("/firms/{f}", get(status))
        .route("/firms/{f}/runs", get(runs))
        .route("/firms/{f}/runs/{run}/journal", get(journal))
        .route("/firms/{f}/runs/{run}/transcripts/{worker}", get(transcript))
        .route("/firms/{f}/runs/{run}/drive/{artifact}/{version}/{*path}", get(drive_file))
        .route("/firms/{f}/runs/{run}/git", get(git_log))
        .route("/firms/{f}/runs/{run}/git/branches", get(git_branches))
        .route("/firms/{f}/runs/{run}/git/log", get(git_ref_log))
        .route("/firms/{f}/runs/{run}/git/tree", get(git_tree))
        .route("/firms/{f}/runs/{run}/git/blob", get(git_blob))
        .route("/firms/{f}/runs/{run}/git/commits/{id}", get(git_commit))
        .route("/firms/{f}/mail", get(inbox))
        .route("/firms/{f}/threads/{id}", get(thread))
        .route("/firms/{f}/drive", get(drive_list))
        .route("/firms/{f}/security", get(security_state))
}

/// The board's writes, in the API and the UI.
pub fn board_routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/firms/{f}/up", post(up))
        .route("/firms/{f}/down", post(down))
        .route("/firms/{f}/pause", post(pause))
        .route("/firms/{f}/mail", post(send))
        .route("/firms/{f}/mail/read", post(mark_read))
        // Base64 grows a file by a third; leave room for the JSON around it.
        .route("/firms/{f}/drive/upload", post(upload).layer(axum::extract::DefaultBodyLimit::max(drive::BOARD_UPLOAD_MAX / 3 * 4 + (64 << 10))))
        .route("/firms/{f}/security/requests/{id}", post(decide))
        .route("/firms/{f}/security/rules/{id}", delete(revoke))
        .route("/firms/{f}/security/rules/{id}/promote", post(promote))
}

/// Writes for the API only.
pub fn api_routes() -> Router<Arc<Daemon>> {
    Router::new().route("/firms/{f}/runs/{run}", delete(prune))
}

async fn list(State(d): D) -> Json<Value> {
    let config = d.config.read().unwrap().clone();
    let out: Vec<Value> = config
        .firms
        .iter()
        .map(|(name, f)| {
            let run = d.office.run(name).ok();
            json!({
                "name": name, "description": f.description, "workers": f.workers.len(),
                "run": run.as_ref().map(|r| r.id.clone()), "state": run.as_ref().map(|r| json!(*r.state.lock().unwrap())),
            })
        })
        .collect();
    Json(json!(out))
}

async fn status(State(d): D, Path(f): Path<String>) -> R<Json<Value>> {
    let config = d.config.read().unwrap().clone();
    let run = d.office.run(&f).ok();
    // A running firm's chart is the one it booted with, whatever the config
    // says now.
    let booted = run.as_ref().filter(|r| !r.ended()).map(|r| r.config.clone());
    let firm = match &booted {
        Some(b) => b,
        None => config.firms.get(&f).ok_or_else(|| anyhow::anyhow!("no firm {f}"))?,
    };
    Ok(Json(json!({
        "name": f,
        "description": firm.description,
        "board": firm.board.name,
        "product_port": firm.product_port,
        "dashboard_port": firm.dashboard_port,
        "chart": firm.workers.iter().map(|w| json!({
            "name": w.name, "role": w.role, "title": firm.title_of(&w.name), "reports_to": w.reports_to,
            "officer": firm.is_officer(&w.name), "avatar": firm.roles.get(&w.role).map(|r| json!(r.avatar)),
            "model": firm.roles.get(&w.role).and_then(|r| super::effective_model(r, &config)),
        })).collect::<Vec<_>>(),
        "run": run.map(|r| r.status()),
    })))
}

async fn runs(State(d): D, Path(f): Path<String>) -> R<Json<Value>> {
    if !d.config.read().unwrap().firms.contains_key(&f) && !super::firm_dir(&d, &f).is_dir() {
        return Err(anyhow::anyhow!("no firm {f}").into());
    }
    Ok(Json(json!(super::list_runs(&d, &f))))
}

#[derive(Deserialize, Default)]
struct Follow {
    #[serde(default)]
    follow: bool,
    /// Byte offset to start from.
    #[serde(default)]
    from: u64,
}

/// A JSON-lines file, whole or followed as server-sent events.
async fn lines(path: PathBuf, q: Follow) -> R<Response> {
    if !q.follow {
        let text = tokio::fs::read_to_string(&path).await.map_err(|_| ApiError(StatusCode::NOT_FOUND, format!("no {}", path.file_name().unwrap_or_default().to_string_lossy())))?;
        return Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], text).into_response());
    }
    Ok(Sse::new(tail(path, q.from)).keep_alive(KeepAlive::default()).into_response())
}

/// Follows a file: complete lines as they are written.
fn tail(path: PathBuf, from: u64) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    futures_util::stream::unfold((path, from, Vec::<u8>::new(), std::collections::VecDeque::<String>::new()), |(path, mut offset, mut partial, mut ready)| async move {
        loop {
            if let Some(line) = ready.pop_front() {
                return Some((Ok(SseEvent::default().data(line)), (path, offset, partial, ready)));
            }
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            if let Ok(mut f) = tokio::fs::File::open(&path).await {
                if f.seek(std::io::SeekFrom::Start(offset)).await.is_ok() {
                    let mut buf = Vec::new();
                    if let Ok(n) = (&mut f).take(4 << 20).read_to_end(&mut buf).await {
                        offset += n as u64;
                        partial.extend_from_slice(&buf);
                        while let Some(i) = partial.iter().position(|c| *c == b'\n') {
                            let line: Vec<u8> = partial.drain(..=i).collect();
                            let s = String::from_utf8_lossy(&line).trim().to_string();
                            if !s.is_empty() {
                                ready.push_back(s);
                            }
                        }
                    }
                }
            }
            if ready.is_empty() {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    })
}

async fn journal(State(d): D, Path((f, run)): Path<(String, String)>, Query(q): Query<Follow>) -> R<Response> {
    let dir = super::run_dir(&d, &f, &run)?;
    lines(dir.join("journal.jsonl"), q).await
}

async fn transcript(State(d): D, Path((f, run, worker)): Path<(String, String, String)>, Query(q): Query<Follow>) -> R<Response> {
    let dir = super::run_dir(&d, &f, &run)?;
    if !zapper_policy::valid_worker_name(&worker) {
        return Err(bad(format!("{worker:?} is not a worker name")));
    }
    lines(dir.join("transcripts").join(format!("{worker}.jsonl")), q).await
}

/// A published file. Served as its own type with a sandboxing CSP, so a
/// published page or SVG can't run script in the UI's origin.
async fn drive_file(State(d): D, Path((f, run, artifact, version, path)): Path<(String, String, String, String, String)>) -> R<Response> {
    let dir = super::run_dir(&d, &f, &run)?;
    let version: u32 = version.trim_start_matches('v').parse().map_err(|_| bad("bad version"))?;
    let (sha, _) = drive::find_file(&dir, &artifact, version, &path).map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{e:#}")))?;
    let data = tokio::fs::read(dir.join("drive").join(&sha)).await.map_err(|_| ApiError(StatusCode::NOT_FOUND, "file missing".into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, drive::content_type(&path)),
            (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; frame-ancestors 'none'"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_DISPOSITION, "inline"),
        ],
        data,
    )
        .into_response())
}

async fn git_log(State(d): D, Path((f, run)): Path<(String, String)>) -> R<Json<Value>> {
    let dir = super::run_dir(&d, &f, &run)?;
    Ok(Json(super::facility::git_log(&dir).await))
}

#[derive(Deserialize)]
struct GitQuery {
    #[serde(rename = "ref", default = "main_ref")]
    r: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    limit: Option<usize>,
}

fn main_ref() -> String {
    "main".into()
}

async fn git_branches(State(d): D, Path((f, run)): Path<(String, String)>) -> R<Json<Value>> {
    Ok(Json(super::git::branches(&super::run_dir(&d, &f, &run)?).await?))
}

async fn git_ref_log(State(d): D, Path((f, run)): Path<(String, String)>, Query(q): Query<GitQuery>) -> R<Json<Value>> {
    Ok(Json(super::git::log(&super::run_dir(&d, &f, &run)?, &q.r, q.limit.unwrap_or(200)).await?))
}

async fn git_tree(State(d): D, Path((f, run)): Path<(String, String)>, Query(q): Query<GitQuery>) -> R<Json<Value>> {
    Ok(Json(super::git::tree(&super::run_dir(&d, &f, &run)?, &q.r, &q.path).await?))
}

async fn git_blob(State(d): D, Path((f, run)): Path<(String, String)>, Query(q): Query<GitQuery>) -> R<Json<Value>> {
    Ok(Json(super::git::blob(&super::run_dir(&d, &f, &run)?, &q.r, &q.path).await?))
}

async fn git_commit(State(d): D, Path((f, run, id)): Path<(String, String, String)>) -> R<Json<Value>> {
    Ok(Json(super::git::commit(&super::run_dir(&d, &f, &run)?, &id).await?))
}

#[derive(Deserialize)]
struct InboxQuery {
    #[serde(rename = "for")]
    who: Option<String>,
    #[serde(default)]
    unread: bool,
}

async fn inbox(State(d): D, Path(f): Path<String>, Query(q): Query<InboxQuery>) -> R<Json<Value>> {
    let run = d.office.run(&f)?;
    let who = q.who.unwrap_or_else(|| BOARD.into());
    let po = run.post.lock().unwrap();
    Ok(Json(json!(po.inbox(&who, q.unread, 500))))
}

async fn thread(State(d): D, Path((f, id)): Path<(String, String)>) -> R<Json<Value>> {
    let run = d.office.run(&f)?;
    let po = run.post.lock().unwrap();
    Ok(Json(po.thread(&run.journal, &id, BOARD)?))
}

async fn drive_list(State(d): D, Path(f): Path<String>) -> R<Json<Value>> {
    let run = d.office.run(&f)?;
    let list = run.drive.lock().unwrap().list(BOARD, None, None, None);
    Ok(Json(json!(list)))
}

async fn security_state(State(d): D, Path(f): Path<String>) -> R<Json<Value>> {
    let run = d.office.run(&f)?;
    Ok(Json(json!({ "requests": security::requests(&run, false), "standing": security::standing(&run) })))
}

async fn up(State(d): D, Path(f): Path<String>) -> R<Json<Value>> {
    let run = super::up(&d, &f).await?;
    Ok(Json(run.status()))
}

async fn down(State(d): D, Path(f): Path<String>) -> R<Json<Value>> {
    let run = d.office.active(&f)?;
    super::down(&d, &run, "firm down").await;
    Ok(Json(json!({ "run": run.id, "state": "ended", "dir": run.dir })))
}

async fn pause(State(d): D, Path(f): Path<String>, text: String) -> R<Json<Value>> {
    let v = body(&text)?;
    let run = d.office.active(&f)?;
    super::pause(&run, v["paused"].as_bool().unwrap_or(true))?;
    Ok(Json(json!({ "state": *run.state.lock().unwrap() })))
}

async fn send(State(d): D, Path(f): Path<String>, text: String) -> R<Json<Value>> {
    let v = body(&text)?;
    let run = d.office.active(&f)?;
    let sent = super::board_send(&run, &v)?;
    Ok(Json(json!(sent)))
}

/// Files from the board's own computer, stored as a shared drive artifact
/// by the board to attach to mail: `{title?, thread?, files: [{name, data}]}`
/// with `data` in base64.
async fn upload(State(d): D, Path(f): Path<String>, text: String) -> R<Json<Value>> {
    use base64::Engine;
    let v: Value = serde_json::from_str(&text).map_err(|e| bad(format!("body: {e}")))?;
    let run = d.office.active(&f)?;
    let mut files = Vec::new();
    for x in v["files"].as_array().into_iter().flatten() {
        let name = x["name"].as_str().ok_or_else(|| bad("each file needs a name"))?;
        let data = base64::engine::general_purpose::STANDARD.decode(x["data"].as_str().unwrap_or("")).map_err(|_| bad(format!("{name}: data is not base64")))?;
        files.push((name.to_string(), data));
    }
    let thread = v["thread"].as_str().map(str::to_string);
    let out = drive::publish_board(&run, files, v["title"].as_str().unwrap_or(""), thread).map_err(|e| bad(e.to_string()))?;
    Ok(Json(out))
}

async fn mark_read(State(d): D, Path(f): Path<String>, text: String) -> R<Json<Value>> {
    let v = body(&text)?;
    let run = d.office.run(&f)?;
    let ids: Vec<String> = v["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect();
    run.post.lock().unwrap().mark_read(BOARD, &ids);
    Ok(Json(json!({ "ok": true })))
}

async fn decide(State(d): D, Path((f, id)): Path<(String, String)>, text: String) -> R<Json<Value>> {
    let v = body(&text)?;
    let run = d.office.active(&f)?;
    let strings = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>());
    let a = security::DecideArgs {
        allow: v["allow"].as_bool().ok_or_else(|| bad("allow is required"))?,
        scope: v["scope"].as_str().unwrap_or("once").to_string(),
        reason: v["reason"].as_str().unwrap_or("decided by the board").to_string(),
        methods: strings("methods"),
        path: v["path"].as_str().map(str::to_string),
        expires_in: v["expires_in"].as_str().map(str::to_string),
    };
    Ok(Json(security::decide(&run, BOARD, &id, a)?))
}

async fn revoke(State(d): D, Path((f, id)): Path<(String, String)>) -> R<Json<Value>> {
    let run = d.office.active(&f)?;
    Ok(Json(security::revoke(&run, BOARD, &id, "revoked by the board")?))
}

/// Copies a standing approval into the running config, at the profile of the
/// role it applies to; `zapper config save` keeps it.
async fn promote(State(d): D, Path((f, id)): Path<(String, String)>) -> R<Json<Value>> {
    let run = d.office.active(&f)?;
    let (scope, rule) = security::promote_target(&run, &id)?;
    let added = d.change(&format!("approval {id} of firm {f} promoted to {scope}"), |c| {
        let mut rule = AnyRule::Net(rule);
        let taken = c.rule_ids();
        rule.set_id((1..).map(|n| format!("net-{n}")).find(|i| !taken.contains(i)).unwrap());
        c.policy_mut(&scope).ok_or_else(|| anyhow::anyhow!("no {scope}"))?.insert(rule.clone(), Some(0));
        Ok(rule)
    })?;
    run.journal.append("access", json!({ "event": "promoted", "rule": id, "to": scope.to_string(), "config_rule": added.id() }));
    Ok(Json(json!({ "promoted": id, "scope": scope.to_string(), "rule": added })))
}

async fn prune(State(d): D, Path((f, run)): Path<(String, String)>) -> R<Json<Value>> {
    super::prune(&d, &f, &run)?;
    Ok(Json(json!({ "removed": run })))
}
