//! Security officers: access requests, decisions, standing approvals, and
//! the traffic digests that let officers watch the firm.
//!
//! In a firm, a request that hits an `ask` rule is refused at once and
//! becomes an access request; an officer (or the board) decides it, and the
//! worker is told by mail.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::sync::atomic::Ordering;

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use zapper_policy::{parse_duration, Action, AnyRule, NetRequest, NetRule, Scope, BOARD};

use super::mail::{Draft, Kind};
use super::{now, prompts, FirmRun, WorkerRt};

/// Requests one worker may have pending.
const MAX_PENDING: usize = 32;
/// How long a `once` approval waits to be used.
const ONCE_SECS: f64 = 600.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Pending,
    Escalated,
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub id: String,
    /// The worker or facility that asked.
    pub worker: String,
    pub title: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub methods: Option<Vec<String>>,
    pub reason: String,
    pub created: f64,
    pub attempts: u32,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub officer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment: Option<String>,
    pub board_asked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Value>,
}

/// An approval or denial that stays: a rule at a worker's or role's layer.
#[derive(Debug, Clone, Serialize)]
pub struct Standing {
    pub rule: NetRule,
    /// `worker:NAME`, `role:NAME` or `once:NAME`.
    pub scope: String,
    pub by: String,
    pub reason: String,
    pub request: Option<String>,
    pub created: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
}

#[derive(Default)]
pub struct Security {
    pub requests: BTreeMap<String, Request>,
    pub standing: BTreeMap<String, Standing>,
    counter: u64,
    rule_counter: u64,
    next_officer: usize,
    /// Hosts seen so far, for "new hosts" in digests.
    seen_hosts: BTreeSet<String>,
    /// Per session, the last request id put into a digest.
    digested: HashMap<String, u64>,
    pub last_digest: f64,
}

fn internal(run: &FirmRun, host: &str) -> bool {
    host == run.domain || host.ends_with(&format!(".{}", run.domain))
}

fn subject_of(r: &Request) -> String {
    format!("Access: {} {}", r.method.as_deref().unwrap_or("CONNECT"), r.host)
}

/// The officer who gets a new request: round robin, never the requester.
fn pick_officer(run: &FirmRun, sec: &mut Security, requester: &str) -> Option<String> {
    let officers: Vec<&String> = run.config.security.officers.iter().filter(|o| *o != requester).collect();
    if officers.is_empty() {
        return None;
    }
    sec.next_officer = (sec.next_officer + 1) % officers.len();
    Some(officers[sec.next_officer].clone())
}

/// Records a request from `worker`; returns its id. Repeated attempts at the
/// same thing join the pending request.
pub fn ask(run: &FirmRun, worker: &str, req: &NetRequest, methods: Option<Vec<String>>, path: Option<String>, reason: &str) -> Result<String> {
    // The post office's lock is never taken while this one is held.
    let assignment = run.post.lock().unwrap().desk(worker).owed.first().map(|o| o.subject.clone());
    let mut sec = run.security.lock().unwrap();
    let method = req.method.clone();
    if let Some(r) = sec.requests.values_mut().find(|r| {
        r.worker == worker && r.host == req.host && r.port == req.port && matches!(r.state, State::Pending | State::Escalated) && (r.method == method || r.method.is_none())
    }) {
        r.attempts += 1;
        if !reason.is_empty() && r.reason.is_empty() {
            r.reason = reason.chars().take(2000).collect();
        }
        return Ok(r.id.clone());
    }
    let pending = sec.requests.values().filter(|r| r.worker == worker && matches!(r.state, State::Pending | State::Escalated)).count();
    if pending >= MAX_PENDING {
        bail!("{worker} has {MAX_PENDING} access requests pending");
    }
    sec.counter += 1;
    let id = format!("r-{}", sec.counter);
    let officer = pick_officer(run, &mut sec, worker);
    let board = officer.is_none() || internal(run, &req.host);
    let r = Request {
        id: id.clone(),
        worker: worker.to_string(),
        title: run.config.title_of(worker),
        host: req.host.clone(),
        port: req.port,
        method,
        path: path.or_else(|| req.path.clone()),
        methods,
        reason: reason.chars().take(2000).collect(),
        created: now(),
        attempts: 1,
        state: if board { State::Escalated } else { State::Pending },
        officer: officer.clone(),
        assignment,
        board_asked: board,
        decision: None,
    };
    run.journal.append("access", json!({ "event": "requested", "request": r }));
    let text = request_text(&r);
    sec.requests.insert(id.clone(), r);
    drop(sec);
    if board {
        run.notice(BOARD, &format!("Access request {id}"), &format!("[Automatic notice] An access request needs the board's decision{}.\n\n{text}\nDecide it in the UI's Security tab.\n", if officer.is_none() { " (the firm has no other officer)" } else { " (the firm's own services)" }));
    } else if let Some(o) = officer.and_then(|o| run.workers.get(&o)) {
        o.access_queue.lock().unwrap().push(id.clone());
        o.wake.notify_one();
    }
    Ok(id)
}

fn request_text(r: &Request) -> String {
    let mut s = format!("Request {}: {} {}:{}{}\nFrom: {} ({})\n", r.id, r.method.as_deref().unwrap_or("CONNECT"), r.host, r.port, r.path.as_deref().unwrap_or(""), r.worker, r.title);
    if !r.reason.is_empty() {
        let _ = writeln!(s, "Reason given:\n{}", r.reason.lines().map(|l| format!("> {l}\n")).collect::<String>());
    }
    s
}

pub fn has_work(_run: &FirmRun, w: &WorkerRt) -> bool {
    w.officer && (!w.access_queue.lock().unwrap().is_empty() || w.digest_due.load(Ordering::Relaxed))
}

/// The officer's part of a wake prompt: pending requests, then a digest.
pub fn take_wake(run: &FirmRun, w: &WorkerRt) -> Option<String> {
    if !w.officer {
        return None;
    }
    let ids: Vec<String> = std::mem::take(&mut *w.access_queue.lock().unwrap());
    let digest = w.digest_due.swap(false, Ordering::Relaxed);
    let mut out = String::new();
    let reqs: Vec<Value> = {
        let sec = run.security.lock().unwrap();
        ids.iter()
            .filter_map(|id| sec.requests.get(id))
            .filter(|r| r.state == State::Pending)
            .map(|r| {
                let mut v = serde_json::to_value(r).unwrap();
                v["recent"] = json!(recent_traffic(run, &r.worker));
                v
            })
            .collect()
    };
    if !reqs.is_empty() {
        out.push_str(&prompts::access_requests(&reqs));
    }
    if digest {
        if let Some(d) = digest_text(run) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&d);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Desk lines for a worker's own pending requests.
pub fn pending_for(run: &FirmRun, worker: &str) -> Vec<String> {
    run.security
        .lock()
        .unwrap()
        .requests
        .values()
        .filter(|r| r.worker == worker && matches!(r.state, State::Pending | State::Escalated))
        .map(|r| format!("  - {} {}:{} ({})", r.method.as_deref().unwrap_or("CONNECT"), r.host, r.port, r.id))
        .collect()
}

fn recent_traffic(run: &FirmRun, worker: &str) -> String {
    let Some(s) = run.workers.get(worker).and_then(|w| w.session()) else { return "none".into() };
    let list = s.requests.lock().unwrap().list();
    let allowed = list.iter().filter(|r| r["action"] == "allow").count();
    let denied = list.len() - allowed;
    let mut hosts: Vec<String> = list.iter().rev().filter_map(|r| r["url"].as_str().map(|u| u.split([':', '/']).next().unwrap_or("").to_string())).collect();
    hosts.dedup();
    hosts.truncate(5);
    format!("{allowed} allowed, {denied} denied; recent hosts: {}", if hosts.is_empty() { "none".into() } else { hosts.join(", ") })
}

#[derive(Debug, Clone, Default)]
pub struct DecideArgs {
    pub allow: bool,
    pub scope: String,
    pub reason: String,
    pub methods: Option<Vec<String>>,
    pub path: Option<String>,
    pub expires_in: Option<String>,
}

/// An officer or the board decides a request.
pub fn decide(run: &FirmRun, by: &str, id: &str, a: DecideArgs) -> Result<Value> {
    let board = by == BOARD;
    if !board && !run.config.is_officer(by) {
        bail!("only security officers and the board decide access requests");
    }
    let mut sec = run.security.lock().unwrap();
    let r = sec.requests.get(id).cloned().ok_or_else(|| anyhow!("no request {id}"))?;
    if !matches!(r.state, State::Pending | State::Escalated) {
        bail!("request {id} is already {:?}", r.state);
    }
    if r.worker == by {
        bail!("you can't decide your own request; another officer or the board will");
    }
    if !board && (r.state == State::Escalated || internal(run, &r.host)) {
        bail!("request {id} is the board's to decide (the firm's own services, or escalated)");
    }
    if a.reason.trim().is_empty() {
        bail!("give a reason; the worker and the board read it");
    }
    let expires = match &a.expires_in {
        Some(e) => Some(now() + parse_duration(e)?.as_secs_f64()),
        None => None,
    };
    let scope = a.scope.as_str();
    if !["once", "worker", "role"].contains(&scope) {
        bail!("scope is once, worker or role");
    }
    let methods = a.methods.clone().or_else(|| r.methods.clone()).unwrap_or_default();
    let path = a.path.clone();
    let rule = NetRule {
        id: None,
        action: if a.allow { Action::Allow } else { Action::Deny },
        host: r.host.clone(),
        ports: vec![r.port],
        methods: methods.iter().map(|m| m.to_ascii_uppercase()).collect(),
        path,
        secrets: vec![],
        set_headers: Default::default(),
        final_: false,
        comment: Some(format!("{by}: {}", a.reason.chars().take(200).collect::<String>())),
    };
    let worker = run.workers.get(&r.worker);
    let mut rule_id = None;
    // A once-denial needs no rule: the request was already refused.
    if a.allow || scope != "once" {
        match scope {
            "role" => {
                let role = worker.map(|w| w.role.clone()).ok_or_else(|| anyhow!("{} has no role", r.worker))?;
                sec.rule_counter += 1;
                let mut rl = rule.clone();
                rl.id = Some(format!("a-net-{}", sec.rule_counter));
                rule_id = rl.id.clone();
                run.roles[&role].write().unwrap().insert(AnyRule::Net(rl.clone()), Some(0));
                sec.standing.insert(rl.id.clone().unwrap(), Standing { rule: rl, scope: format!("role:{role}"), by: by.into(), reason: a.reason.clone(), request: Some(id.into()), created: now(), expires });
            }
            _ => {
                let s = worker
                    .and_then(|w| w.session())
                    .or_else(|| run.facility.session_of(&r.worker))
                    .ok_or_else(|| anyhow!("{} has no session", r.worker))?;
                // An id unique in the run, not just in the session: the
                // standing approvals of all workers share one list.
                sec.rule_counter += 1;
                let mut rl = rule.clone();
                rl.id = Some(format!("a-net-{}", sec.rule_counter));
                s.policy.write().unwrap().insert(AnyRule::Net(rl.clone()), Some(0));
                rule_id = rl.id.clone();
                let expires = if scope == "once" { Some(now() + ONCE_SECS) } else { expires };
                sec.standing.insert(rl.id.clone().unwrap(), Standing { rule: rl, scope: format!("{scope}:{}", r.worker), by: by.into(), reason: a.reason.clone(), request: Some(id.into()), created: now(), expires });
            }
        }
    }
    let decision = json!({ "by": by, "allow": a.allow, "scope": scope, "reason": a.reason, "rule": rule_id, "expires": expires, "methods": methods, "path": a.path, "at": now() });
    let req = sec.requests.get_mut(id).unwrap();
    req.state = if a.allow { State::Allowed } else { State::Denied };
    req.decision = Some(decision.clone());
    let r = req.clone();
    drop(sec);
    run.journal.append("access", json!({ "event": "decided", "request": r }));
    // Tell the worker.
    if run.workers.contains_key(&r.worker) {
        let what = if a.allow { "allowed" } else { "denied" };
        let scope_text = match scope {
            "once" => "once".to_string(),
            "worker" => "for you".into(),
            _ => "for your role".into(),
        };
        let mut body = format!(
            "Your request {} to {} {}:{}{} was {what}.\nScope: {scope_text}{}.\n",
            r.id,
            r.method.as_deref().unwrap_or("CONNECT"),
            r.host,
            r.port,
            r.path.as_deref().unwrap_or(""),
            expires.map(|e| format!(", until {}", prompts::when(e))).unwrap_or_default()
        );
        if a.allow && scope == "once" {
            body.push_str("Retry within 10 minutes; the approval covers one matching connection or request.\n");
        } else if a.allow {
            body.push_str("Retry now.\n");
        }
        let _ = writeln!(body, "\nDecided by {by}. Reason:\n{}", a.reason.lines().map(|l| format!("> {l}\n")).collect::<String>());
        let mut d = Draft::new(by, vec![r.worker.clone()], &subject_of(&r), &body, Kind::Reply);
        d.automatic = true;
        if let Err(e) = run.send(d) {
            tracing::warn!("access decision mail: {e:#}");
        }
    }
    Ok(decision)
}

/// An officer hands a request to the board.
pub fn escalate(run: &FirmRun, by: &str, id: &str, recommendation: &str) -> Result<Value> {
    if !run.config.is_officer(by) {
        bail!("only security officers escalate");
    }
    let mut sec = run.security.lock().unwrap();
    let r = sec.requests.get_mut(id).ok_or_else(|| anyhow!("no request {id}"))?;
    if r.state != State::Pending {
        bail!("request {id} is {:?}", r.state);
    }
    r.state = State::Escalated;
    r.board_asked = true;
    r.decision = Some(json!({ "escalated_by": by, "recommendation": recommendation }));
    let text = request_text(r);
    let r = r.clone();
    drop(sec);
    run.journal.append("access", json!({ "event": "escalated", "request": r, "by": by }));
    run.notice(BOARD, &format!("Escalated: {}", subject_of(&r)), &format!("[Automatic notice] {by} escalated an access request to the board.\n\n{text}\n{by}'s recommendation:\n{}\nDecide it in the UI's Security tab.\n", recommendation.lines().map(|l| format!("> {l}\n")).collect::<String>()));
    Ok(json!({ "escalated": id }))
}

/// Removes a standing approval.
pub fn revoke(run: &FirmRun, by: &str, rule: &str, reason: &str) -> Result<Value> {
    if by != BOARD && !run.config.is_officer(by) {
        bail!("only security officers and the board revoke approvals");
    }
    let st = run.security.lock().unwrap().standing.remove(rule).ok_or_else(|| anyhow!("no standing approval {rule}"))?;
    remove_rule(run, &st);
    run.journal.append("access", json!({ "event": "revoked", "rule": rule, "standing": st, "by": by, "reason": reason }));
    Ok(json!({ "revoked": rule }))
}

fn remove_rule(run: &FirmRun, st: &Standing) {
    let Some(id) = st.rule.id.as_deref() else { return };
    match st.scope.split_once(':') {
        Some(("role", role)) => {
            if let Some(p) = run.roles.get(role) {
                p.write().unwrap().remove(id);
            }
        }
        Some((_, worker)) => {
            if let Some(s) = run.workers.get(worker).and_then(|w| w.session()).or_else(|| run.facility.session_of(worker)) {
                s.policy.write().unwrap().remove(id);
            }
        }
        None => {}
    }
}

/// Called by the proxy when a rule allowed a request: `once` approvals are
/// used up.
pub fn used(run: &FirmRun, rule: &str) {
    let mut sec = run.security.lock().unwrap();
    let once = sec.standing.get(rule).is_some_and(|s| s.scope.starts_with("once:"));
    if once {
        if let Some(st) = sec.standing.remove(rule) {
            drop(sec);
            remove_rule(run, &st);
            run.journal.append("access", json!({ "event": "used", "rule": rule }));
        }
    }
}

/// Standing approvals with how many requests each allowed.
pub fn standing(run: &FirmRun) -> Vec<Value> {
    let sec = run.security.lock().unwrap();
    let mut uses: HashMap<String, u64> = HashMap::new();
    for w in run.workers.values() {
        if let Some(s) = w.session() {
            for r in s.requests.lock().unwrap().list() {
                if let Some(id) = r["rule"].as_str() {
                    *uses.entry(id.to_string()).or_default() += 1;
                }
            }
        }
    }
    sec.standing
        .iter()
        .map(|(id, st)| {
            let mut v = serde_json::to_value(st).unwrap();
            v["id"] = json!(id);
            v["uses"] = json!(uses.get(id).copied().unwrap_or(0));
            v
        })
        .collect()
}

pub fn requests(run: &FirmRun, pending_only: bool) -> Vec<Value> {
    let sec = run.security.lock().unwrap();
    sec.requests.values().filter(|r| !pending_only || matches!(r.state, State::Pending | State::Escalated)).map(|r| serde_json::to_value(r).unwrap()).collect()
}

/// The firm's traffic, newest first.
pub fn traffic(run: &FirmRun, worker: Option<&str>, host: Option<&str>, since: Option<f64>, limit: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for w in run.workers.values() {
        if worker.is_some_and(|x| x != w.name) {
            continue;
        }
        let Some(s) = w.session() else { continue };
        for mut r in s.requests.lock().unwrap().list() {
            if since.is_some_and(|t| r["ts"].as_f64().unwrap_or(0.0) < t) {
                continue;
            }
            if host.is_some_and(|h| !r["url"].as_str().unwrap_or("").starts_with(h)) {
                continue;
            }
            r["worker"] = json!(w.name);
            out.push(r);
        }
    }
    out.sort_by(|a, b| b["ts"].as_f64().unwrap_or(0.0).total_cmp(&a["ts"].as_f64().unwrap_or(0.0)));
    out.truncate(limit);
    out
}

/// A digest on request, from the `traffic_digest` tool.
pub fn take_wake_digest(run: &FirmRun) -> Option<String> {
    digest_text(run)
}

fn digest_text(run: &FirmRun) -> Option<String> {
    let mut sec = run.security.lock().unwrap();
    let from = sec.last_digest;
    let to = now();
    sec.last_digest = to;
    let mut total = 0;
    let mut allowed = 0;
    let mut new_hosts: BTreeMap<String, (String, u64, BTreeSet<String>, u64, u64)> = BTreeMap::new();
    let mut uploads: Vec<(u64, String, String)> = Vec::new();
    for w in run.workers.values() {
        let Some(s) = w.session() else { continue };
        let last = sec.digested.get(&s.id).copied().unwrap_or(0);
        let mut max_id = last;
        for r in s.requests.lock().unwrap().list() {
            let rid = r["id"].as_u64().unwrap_or(0);
            if rid <= last {
                continue;
            }
            max_id = max_id.max(rid);
            total += 1;
            if r["action"] == "allow" {
                allowed += 1;
            }
            let url = r["url"].as_str().unwrap_or("").to_string();
            let host = url.split([':', '/']).next().unwrap_or("").to_string();
            let up = r["req_size"].as_u64().unwrap_or(0);
            let down = r["resp_size"].as_u64().unwrap_or(0);
            if !sec.seen_hosts.contains(&host) {
                let e = new_hosts.entry(host.clone()).or_insert((w.name.clone(), 0, BTreeSet::new(), 0, 0));
                e.1 += 1;
                e.2.insert(r["method"].as_str().unwrap_or("").to_string());
                e.3 += up;
                e.4 += down;
            }
            if up > 0 {
                uploads.push((up, w.name.clone(), url));
            }
        }
        sec.digested.insert(s.id.clone(), max_id);
    }
    sec.seen_hosts.extend(new_hosts.keys().cloned());
    if total == 0 {
        return None;
    }
    uploads.sort_by(|a, b| b.0.cmp(&a.0));
    let mut s = format!("Traffic digest {} – {}: {total} requests, {allowed} allowed, {} denied.\n\nNew hosts this period:\n", prompts::when(from), prompts::when(to), total - allowed);
    if new_hosts.is_empty() {
        s.push_str("  none\n");
    }
    for (h, (w, n, m, up, down)) in &new_hosts {
        let _ = writeln!(s, "  {h} — first by {w}, {n} requests, {}, {up} B up / {down} B down", m.iter().cloned().collect::<Vec<_>>().join(","));
    }
    s.push_str("Largest uploads:\n");
    if uploads.is_empty() {
        s.push_str("  none\n");
    }
    for (b, w, u) in uploads.iter().take(5) {
        let _ = writeln!(s, "  {w} → {u}, {b} B");
    }
    let rules: Vec<String> = sec.standing.iter().map(|(id, st)| format!("  {id} ({}) {} {}", st.scope, st.rule.host, if st.rule.action == Action::Allow { "allow" } else { "deny" })).collect();
    let _ = writeln!(s, "Standing approvals:\n{}", if rules.is_empty() { "  none".to_string() } else { rules.join("\n") });
    s.push_str("\nLook for traffic that doesn't fit a worker's assignment, unusual uploads, or\napprovals broader than needed. Use traffic() for detail. Revoke or narrow with\naccess_revoke; mail the worker, its manager or the board if something needs an\nexplanation. If nothing stands out, end your turn without mail.\n");
    drop(sec);
    run.journal.append("digest", json!({ "requests": total, "allowed": allowed, "new_hosts": new_hosts.keys().collect::<Vec<_>>() }));
    Some(s)
}

/// Expiries, unanswered requests, and digests.
pub fn tick(run: &FirmRun, last_digest: &mut f64) {
    let t = now();
    // Expired approvals.
    let expired: Vec<(String, Standing)> = {
        let mut sec = run.security.lock().unwrap();
        let ids: Vec<String> = sec.standing.iter().filter(|(_, s)| s.expires.is_some_and(|e| t > e)).map(|(k, _)| k.clone()).collect();
        ids.into_iter().filter_map(|id| sec.standing.remove(&id).map(|s| (id, s))).collect()
    };
    for (id, st) in expired {
        remove_rule(run, &st);
        run.journal.append("access", json!({ "event": "expired", "rule": id }));
    }
    // Requests no officer answered in time go to the board as well.
    let within = parse_duration(&run.config.security.answer_within).map(|d| d.as_secs_f64()).unwrap_or(900.0);
    let late: Vec<Request> = {
        let mut sec = run.security.lock().unwrap();
        sec.requests
            .values_mut()
            .filter(|r| r.state == State::Pending && !r.board_asked && t - r.created > within)
            .map(|r| {
                r.board_asked = true;
                r.clone()
            })
            .collect()
    };
    for r in late {
        run.journal.append("access", json!({ "event": "board_asked", "request": r.id }));
        run.notice(BOARD, &format!("Unanswered: {}", subject_of(&r)), &format!("[Automatic notice] No security officer answered this request within {}; the board can decide it too.\n\n{}\nDecide it in the UI's Security tab.\n", run.config.security.answer_within, request_text(&r)));
    }
    // Digests: every few minutes, or sooner after many requests.
    let every = run.config.security.digest_every.minutes as f64 * 60.0;
    let threshold = run.config.security.digest_every.requests as usize;
    let new_requests: usize = {
        let sec = run.security.lock().unwrap();
        run.workers
            .values()
            .filter_map(|w| w.session())
            .map(|s| {
                let last = sec.digested.get(&s.id).copied().unwrap_or(0);
                s.requests.lock().unwrap().list().iter().filter(|r| r["id"].as_u64().unwrap_or(0) > last).count()
            })
            .sum()
    };
    if new_requests > 0 && (t - *last_digest >= every.max(30.0) || (threshold > 0 && new_requests >= threshold)) {
        *last_digest = t;
        for o in &run.config.security.officers {
            if let Some(w) = run.workers.get(o) {
                w.digest_due.store(true, Ordering::Relaxed);
                w.wake.notify_one();
            }
        }
    }
}

/// Promotes a standing approval into the running config: the rule goes to
/// the role's profile.
pub fn promote_target(run: &FirmRun, rule: &str) -> Result<(Scope, NetRule)> {
    let sec = run.security.lock().unwrap();
    let st = sec.standing.get(rule).ok_or_else(|| anyhow!("no standing approval {rule}"))?;
    let role = match st.scope.split_once(':') {
        Some(("role", r)) => r.to_string(),
        Some((_, w)) => run.workers.get(w).map(|x| x.role.clone()).ok_or_else(|| anyhow!("{w} isn't a worker"))?,
        None => bail!("bad scope"),
    };
    let profile = run.config.roles[&role].profile.clone();
    let mut r = st.rule.clone();
    r.id = None;
    Ok((Scope::Profile(profile), r))
}
