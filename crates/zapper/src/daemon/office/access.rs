//! The credentials subsystem: who may use the firm's resources (the git
//! repository, the cluster, the shared drive's spaces) and how far, as
//! grants the policy owners decide. A worker asks with a reason; the
//! security officer decides, escalating to the CEO, and the CEO to the
//! board; nobody decides their own request. The platform enforces every
//! grant (git on the host, the cluster with RBAC, the drive on the host),
//! grants expire, and leaving the firm revokes them all. Every step is a
//! `credentials` event in the journal.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zapper_policy::{parse_duration, BOARD};

use super::{now, FirmRun};
use crate::daemon::Daemon;

/// How far a grant reaches. For git: read is fetch, write is push to
/// branches, admin is also `main` and tags. For the cluster: read and write
/// are the `dev` namespace, admin is also `prod`. For a drive space: read
/// is list and fetch, write is also publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    None,
    Read,
    Write,
    Admin,
}

impl Level {
    pub fn parse(s: &str) -> Result<Level> {
        match s.trim() {
            "none" => Ok(Level::None),
            "read" => Ok(Level::Read),
            "write" => Ok(Level::Write),
            "admin" => Ok(Level::Admin),
            other => bail!("level {other:?}: none, read, write or admin"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::None => "none",
            Level::Read => "read",
            Level::Write => "write",
            Level::Admin => "admin",
        }
    }
}

/// A resource's key: `git`, `cluster` or `drive:<space>`.
pub fn resource(run: &FirmRun, r: &str) -> Result<String> {
    let r = r.trim();
    match r {
        "git" if run.config.git.is_some() => Ok(r.into()),
        "cluster" if !run.config.facilities.is_empty() => Ok(r.into()),
        "git" | "cluster" => bail!("this firm has no {r}"),
        _ => match r.strip_prefix("drive:") {
            Some(space) if run.config.credentials.spaces.iter().any(|s| s == space) => Ok(r.into()),
            Some(space) => bail!(
                "no drive space {space:?} (spaces: {})",
                run.config.credentials.spaces.join(", ")
            ),
            None => bail!("resource {r:?}: git, cluster, or drive:<space>"),
        },
    }
}

/// The highest level a resource has: drive spaces stop at write.
fn max_level(resource: &str) -> Level {
    if resource.starts_with("drive:") {
        Level::Write
    } else {
        Level::Admin
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Grant {
    pub worker: String,
    pub resource: String,
    pub level: Level,
    pub by: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub granted: f64,
    /// None: while they hold the role (the policy owners' own access).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(skip)]
    warned: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub id: String,
    pub worker: String,
    pub resource: String,
    pub level: Level,
    pub reason: String,
    pub created: f64,
    /// `pending`, `escalated` (to the CEO or the board), `granted`, `denied`.
    pub state: String,
    /// Who decides it now; None is the board.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Value>,
}

#[derive(Default)]
pub struct Access {
    /// By worker, then resource.
    grants: BTreeMap<String, BTreeMap<String, Grant>>,
    pub requests: BTreeMap<String, Request>,
    counter: u64,
}

/// The policy owners: the CEO and the security officers.
pub fn owners(run: &FirmRun) -> Vec<String> {
    let chart = run.chart();
    let mut v: Vec<String> = chart.root().map(str::to_string).into_iter().collect();
    for o in &chart.security.officers {
        if !v.contains(o) {
            v.push(o.clone());
        }
    }
    v
}

pub fn is_owner(run: &FirmRun, w: &str) -> bool {
    owners(run).iter().any(|o| o == w)
}

/// A worker's level on a resource now (expired grants count as none).
pub fn level(run: &FirmRun, worker: &str, resource: &str) -> Level {
    let a = run.access.lock().unwrap();
    a.grants
        .get(worker)
        .and_then(|g| g.get(resource))
        .filter(|g| g.expires.is_none_or(|e| e > now()))
        .map(|g| g.level)
        .unwrap_or(Level::None)
}

/// Whether a worker may read a drive space (the board reads everything).
pub fn can_read_space(run: &FirmRun, who: &str, space: &str) -> bool {
    who == BOARD || level(run, who, &format!("drive:{space}")) >= Level::Read
}

pub fn can_write_space(run: &FirmRun, who: &str, space: &str) -> bool {
    who == BOARD || level(run, who, &format!("drive:{space}")) >= Level::Write
}

fn every_resource(run: &FirmRun) -> Vec<String> {
    let mut v = Vec::new();
    if run.config.git.is_some() {
        v.push("git".to_string());
    }
    if !run.config.facilities.is_empty() {
        v.push("cluster".to_string());
    }
    v.extend(
        run.config
            .credentials
            .spaces
            .iter()
            .map(|s| format!("drive:{s}")),
    );
    v
}

/// Puts a grant in place: recorded, enforced, and the worker told.
#[allow(clippy::too_many_arguments)]
fn put(
    d: &Arc<Daemon>,
    run: &Arc<FirmRun>,
    worker: &str,
    resource: &str,
    level: Level,
    by: &str,
    note: &str,
    expires: Option<f64>,
    request: Option<&str>,
    tell: bool,
) {
    let g = Grant {
        worker: worker.into(),
        resource: resource.into(),
        level,
        by: by.into(),
        note: note.trim().chars().take(2000).collect(),
        granted: now(),
        expires,
        request: request.map(str::to_string),
        warned: false,
    };
    run.journal
        .append("credentials", json!({ "event": "granted", "grant": g }));
    run.access
        .lock()
        .unwrap()
        .grants
        .entry(worker.into())
        .or_default()
        .insert(resource.into(), g);
    enforce(d, run, worker, resource);
    if tell {
        let until = expires
            .map(|e| format!(", until {}", super::prompts::when(e)))
            .unwrap_or_default();
        run.notice(
            worker,
            &format!("Access granted: {resource} {}", level.name()),
            &format!(
                "[Automatic notice] {by} granted you {} on {resource}{until}{}.\n",
                level.name(),
                if note.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", note.trim())
                }
            ),
        );
    }
}

/// Takes a grant back.
fn take(
    d: &Arc<Daemon>,
    run: &Arc<FirmRun>,
    worker: &str,
    resource: &str,
    by: &str,
    reason: &str,
    tell: bool,
) -> bool {
    let had = run
        .access
        .lock()
        .unwrap()
        .grants
        .get_mut(worker)
        .and_then(|g| g.remove(resource))
        .is_some();
    if !had {
        return false;
    }
    run.journal.append("credentials", json!({ "event": "revoked", "worker": worker, "resource": resource, "by": by, "reason": reason.trim() }));
    enforce(d, run, worker, resource);
    if tell && run.rt(worker).is_some() {
        run.notice(worker, &format!("Access revoked: {resource}"), &format!("[Automatic notice] Your access to {resource} was revoked by {by}: {}. Ask again with credentials_request if your work needs it.\n", reason.trim()));
    }
    true
}

/// Makes the platform match the grant: the cluster's RBAC and the worker's
/// kubeconfig, and the network rules for git and the cluster. git and the
/// drive are checked on every request, so they need nothing here.
fn enforce(d: &Arc<Daemon>, run: &Arc<FirmRun>, worker: &str, resource: &str) {
    if resource == "git" || resource == "cluster" {
        super::facility::network_follows_grant(run, worker, resource, level(run, worker, resource));
    }
    if resource == "cluster" {
        let (d, run, worker) = (d.clone(), run.clone(), worker.to_string());
        tokio::spawn(async move {
            let lvl = level(&run, &worker, "cluster");
            let expires = run
                .access
                .lock()
                .unwrap()
                .grants
                .get(&worker)
                .and_then(|g| g.get("cluster"))
                .and_then(|g| g.expires);
            if let Err(e) = super::facility::cluster_access(&d, &run, &worker, lvl, expires).await {
                tracing::warn!(firm = %run.firm, %worker, "cluster access: {e:#}");
                run.journal.append("credentials", json!({ "event": "enforce_failed", "worker": worker, "resource": "cluster", "error": format!("{e:#}") }));
            }
        });
    }
}

/// Puts a worker's grants in place again: their VM came up after them.
pub fn reapply(d: &Arc<Daemon>, run: &Arc<FirmRun>, worker: &str) {
    for r in ["git", "cluster"] {
        if level(run, worker, r) > Level::None {
            enforce(d, run, worker, r);
        }
    }
}

/// The policy owners' own access: admin on git and the cluster, write on
/// every drive space, for as long as they hold the role.
pub fn owner_access(d: &Arc<Daemon>, run: &Arc<FirmRun>, worker: &str, by: &str) {
    for r in every_resource(run) {
        let lvl = max_level(&r);
        put(
            d,
            run,
            worker,
            &r,
            lvl,
            by,
            "a policy owner",
            None,
            None,
            false,
        );
    }
}

/// Everything a worker holds, revoked: they are leaving, or stepping down.
pub fn revoke_all(d: &Arc<Daemon>, run: &Arc<FirmRun>, worker: &str, by: &str, reason: &str) {
    let held: Vec<String> = run
        .access
        .lock()
        .unwrap()
        .grants
        .get(worker)
        .map(|g| g.keys().cloned().collect())
        .unwrap_or_default();
    for r in held {
        take(d, run, worker, &r, by, reason, false);
    }
    // Their own open requests lapse.
    let mut a = run.access.lock().unwrap();
    for q in a
        .requests
        .values_mut()
        .filter(|q| q.worker == worker && matches!(q.state.as_str(), "pending" | "escalated"))
    {
        q.state = "denied".into();
        q.decision = Some(json!({ "by": by, "note": reason, "at": now() }));
    }
}

/// Who decides a request first: a security officer other than the
/// requester; the CEO for admin on git or the cluster, or when no officer
/// can; the board for the CEO's own.
fn first_decider(run: &FirmRun, worker: &str, resource: &str, level: Level) -> Option<String> {
    let chart = run.chart();
    let ceo = chart.root().map(str::to_string);
    let to_ceo = || ceo.clone().filter(|c| c != worker);
    if level == Level::Admin && !resource.starts_with("drive:") {
        return to_ceo();
    }
    chart
        .security
        .officers
        .iter()
        .find(|o| *o != worker && Some(*o) != ceo.as_ref())
        .cloned()
        .or_else(to_ceo)
}

/// One step up: from an officer to the CEO, from the CEO to the board.
fn next_decider(run: &FirmRun, from: Option<&str>, worker: &str) -> Option<String> {
    let ceo = run.chart().root().map(str::to_string);
    match from {
        Some(f) if Some(f) != ceo.as_deref() => ceo.filter(|c| c != worker),
        _ => None,
    }
}

fn request_text(q: &Request) -> String {
    format!(
        "Credential request {} — {} asks for {} on {}.\nReason (the requester's words):\n{}\nDecide with credentials_decide(request: \"{}\", grant, level?, expires_in?, note), or hand it up with credentials_escalate.\n",
        q.id,
        q.worker,
        q.level.name(),
        q.resource,
        q.reason.lines().map(|l| format!("> {l}\n")).collect::<String>(),
        q.id
    )
}

fn notify_decider(run: &FirmRun, q: &Request, extra: &str) {
    match &q.decider {
        Some(d) => run.notice(d, &format!("Credential request {}: {} {} for {}", q.id, q.resource, q.level.name(), q.worker), &format!("[Automatic notice] {extra}{}", request_text(q))),
        None => run.notice(BOARD, &format!("Credential request {}: {} {} for {}", q.id, q.resource, q.level.name(), q.worker), &format!("[Automatic notice] {extra}A credential request needs the board's decision.\n\n{}\nDecide it in the UI's Security tab, or with `zapper firm credentials`.\n", request_text(q))),
    }
}

/// A worker asks for access; returns the request's id.
pub fn request(
    run: &FirmRun,
    worker: &str,
    resource: &str,
    level: Level,
    reason: &str,
) -> Result<String> {
    let resource = self::resource(run, resource)?;
    if level == Level::None || level > max_level(&resource) {
        bail!(
            "{resource} levels: read, write{}",
            if max_level(&resource) == Level::Admin {
                ", admin"
            } else {
                ""
            }
        );
    }
    if reason.trim().is_empty() {
        bail!("give a reason: what your work needs it for");
    }
    if self::level(run, worker, &resource) >= level {
        bail!(
            "you already have {} on {resource}",
            self::level(run, worker, &resource).name()
        );
    }
    let decider = first_decider(run, worker, &resource, level);
    let q = {
        let mut a = run.access.lock().unwrap();
        if let Some(q) = a.requests.values().find(|q| {
            q.worker == worker
                && q.resource == resource
                && matches!(q.state.as_str(), "pending" | "escalated")
        }) {
            return Ok(q.id.clone());
        }
        a.counter += 1;
        let q = Request {
            id: format!("p-{}", a.counter),
            worker: worker.into(),
            resource,
            level,
            reason: reason.trim().chars().take(2000).collect(),
            created: now(),
            state: if decider.is_some() {
                "pending"
            } else {
                "escalated"
            }
            .into(),
            decider,
            decision: None,
        };
        a.requests.insert(q.id.clone(), q.clone());
        q
    };
    run.journal
        .append("credentials", json!({ "event": "requested", "request": q }));
    notify_decider(run, &q, "");
    Ok(q.id)
}

/// The decider (or the CEO above them, or the board) grants or denies.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    d: &Arc<Daemon>,
    run: &Arc<FirmRun>,
    by: &str,
    id: &str,
    grant: bool,
    level: Option<Level>,
    expires_in: Option<&str>,
    note: &str,
) -> Result<Value> {
    let q = run
        .access
        .lock()
        .unwrap()
        .requests
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("no credential request {id}"))?;
    if !matches!(q.state.as_str(), "pending" | "escalated") {
        bail!("request {id} was already {}", q.state);
    }
    if q.worker == by {
        bail!("you can't decide your own request");
    }
    let ceo = run.chart().root().map(str::to_string);
    let allowed = by == BOARD
        || q.decider.as_deref() == Some(by)
        || (ceo.as_deref() == Some(by) && q.decider.is_some());
    if !allowed {
        bail!(
            "request {id} is {}'s to decide",
            q.decider.as_deref().unwrap_or("the board's")
        );
    }
    if !grant && note.trim().is_empty() {
        bail!("give a reason for the denial; the requester reads it");
    }
    let lvl = level.unwrap_or(q.level);
    if grant && (lvl == Level::None || lvl > q.level) {
        bail!("grant at most what was asked ({})", q.level.name());
    }
    let expires = match expires_in {
        Some(e) => Some(now() + parse_duration(e)?.as_secs_f64()),
        None => Some(now() + parse_duration(&run.config.credentials.expiry)?.as_secs_f64()),
    };
    let decision = json!({ "by": by, "grant": grant, "level": lvl, "expires": if grant { expires } else { None }, "note": note.trim(), "at": now() });
    {
        let mut a = run.access.lock().unwrap();
        let r = a.requests.get_mut(id).expect("request");
        r.state = if grant { "granted" } else { "denied" }.into();
        r.decision = Some(decision.clone());
    }
    run.journal.append("credentials", json!({ "event": "decided", "id": id, "worker": q.worker, "resource": q.resource, "decision": decision }));
    if grant {
        put(
            d,
            run,
            &q.worker,
            &q.resource,
            lvl,
            by,
            note,
            expires,
            Some(id),
            true,
        );
    } else {
        run.notice(
            &q.worker,
            &format!("Access denied: {}", q.resource),
            &format!(
                "[Automatic notice] {by} denied your request {id} for {} on {}: {}\n",
                q.level.name(),
                q.resource,
                note.trim()
            ),
        );
    }
    Ok(
        json!({ "request": id, "state": if grant { "granted" } else { "denied" }, "level": lvl, "expires": expires }),
    )
}

/// The decider hands a request up: an officer to the CEO, the CEO to the
/// board.
pub fn escalate(run: &FirmRun, by: &str, id: &str, recommendation: &str) -> Result<Value> {
    let q = run
        .access
        .lock()
        .unwrap()
        .requests
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("no credential request {id}"))?;
    if q.decider.as_deref() != Some(by) {
        bail!("request {id} isn't yours to hand up");
    }
    let next = next_decider(run, Some(by), &q.worker);
    let q = {
        let mut a = run.access.lock().unwrap();
        let r = a.requests.get_mut(id).expect("request");
        r.decider = next;
        r.state = "escalated".into();
        r.decision = Some(json!({ "escalated_by": by, "recommendation": recommendation.trim() }));
        r.clone()
    };
    run.journal.append("credentials", json!({ "event": "escalated", "id": id, "by": by, "to": q.decider.clone().unwrap_or_else(|| BOARD.into()), "recommendation": recommendation.trim() }));
    notify_decider(
        run,
        &q,
        &format!(
            "{by} handed this up to you, recommending:\n{}\n",
            recommendation
                .trim()
                .lines()
                .map(|l| format!("> {l}\n"))
                .collect::<String>()
        ),
    );
    Ok(json!({ "request": id, "to": q.decider.unwrap_or_else(|| BOARD.into()) }))
}

/// A policy owner (or the board) takes a grant back.
pub fn revoke(
    d: &Arc<Daemon>,
    run: &Arc<FirmRun>,
    by: &str,
    worker: &str,
    resource: &str,
    reason: &str,
) -> Result<Value> {
    if by != BOARD && !is_owner(run, by) {
        bail!("only the security officers, the CEO and the board revoke access");
    }
    if reason.trim().is_empty() {
        bail!("give a reason");
    }
    if is_owner(run, worker) && by != BOARD {
        bail!("{worker} is a policy owner; only the board changes their access");
    }
    let resource = self::resource(run, resource)?;
    if !take(d, run, worker, &resource, by, reason, true) {
        bail!("{worker} has no grant on {resource}");
    }
    Ok(json!({ "worker": worker, "resource": resource, "revoked": true }))
}

/// The board grants directly.
pub fn board_grant(
    d: &Arc<Daemon>,
    run: &Arc<FirmRun>,
    worker: &str,
    resource: &str,
    level: Level,
    expires_in: Option<&str>,
    note: &str,
) -> Result<Value> {
    if run.rt(worker).is_none() {
        bail!("no worker {worker}");
    }
    let resource = self::resource(run, resource)?;
    if level == Level::None || level > max_level(&resource) {
        bail!("{resource} levels go up to {}", max_level(&resource).name());
    }
    let ttl = expires_in
        .map(str::to_string)
        .unwrap_or_else(|| run.config.credentials.expiry.clone());
    let expires = now() + parse_duration(&ttl)?.as_secs_f64();
    put(
        d,
        run,
        worker,
        &resource,
        level,
        BOARD,
        note,
        Some(expires),
        None,
        true,
    );
    Ok(json!({ "worker": worker, "resource": resource, "level": level, "expires": expires }))
}

/// A worker and a resource they hold.
type Held = (String, String);

/// Grants that lapse, and a warning before they do.
pub fn tick(d: &Arc<Daemon>, run: &Arc<FirmRun>) {
    let t = now();
    // (worker, resource) that lapsed; and those to warn, with their expiry.
    let (expired, warn): (Vec<Held>, Vec<(Held, f64)>) = {
        let mut a = run.access.lock().unwrap();
        let mut expired = Vec::new();
        let mut warn = Vec::new();
        for (w, gs) in a.grants.iter_mut() {
            for (r, g) in gs.iter_mut() {
                let Some(e) = g.expires else { continue };
                if e <= t {
                    expired.push((w.clone(), r.clone()));
                } else if !g.warned && e - t < ((e - g.granted) / 10.0).clamp(60.0, 3600.0) {
                    g.warned = true;
                    warn.push(((w.clone(), r.clone()), e));
                }
            }
        }
        (expired, warn)
    };
    for (w, r) in expired {
        take(d, run, &w, &r, "office", "the grant expired", true);
    }
    for ((w, r), e) in warn {
        run.notice(&w, &format!("Access to {r} expires soon"), &format!("[Automatic notice] Your access to {r} expires at {}. If your work still needs it, ask again with credentials_request.\n", super::prompts::when(e)));
    }
}

/// A worker's own grants and requests, and what there is to ask for.
pub fn mine(run: &FirmRun, worker: &str) -> Value {
    let a = run.access.lock().unwrap();
    let grants: Vec<&Grant> = a
        .grants
        .get(worker)
        .map(|g| g.values().collect())
        .unwrap_or_default();
    let requests: Vec<&Request> = a.requests.values().filter(|q| q.worker == worker).collect();
    json!({
        "grants": grants, "requests": requests,
        "resources": every_resource(run),
        "levels": { "git": "read (fetch), write (push branches), admin (also main and tags)", "cluster": "read (view dev), write (deploy to dev), admin (also prod)", "drive:<space>": "read (list, fetch), write (also publish)" },
    })
}

/// Every grant and request, for the policy owners, the API and the UI.
pub fn all(run: &FirmRun) -> Value {
    let a = run.access.lock().unwrap();
    let grants: Vec<&Grant> = a.grants.values().flat_map(|g| g.values()).collect();
    let requests: Vec<&Request> = a.requests.values().collect();
    json!({ "grants": grants, "requests": requests, "resources": every_resource(run), "owners": owners(run) })
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn levels_order() {
        assert!(
            Level::None < Level::Read && Level::Read < Level::Write && Level::Write < Level::Admin
        );
        assert_eq!(Level::parse("write").unwrap(), Level::Write);
        assert!(Level::parse("root").is_err());
        assert_eq!(serde_json::to_value(Level::Admin).unwrap(), "admin");
    }
}
