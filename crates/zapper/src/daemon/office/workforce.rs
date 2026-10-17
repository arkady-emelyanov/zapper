//! A firm's workforce and its money. With a run budget (`budget.seed`), the
//! board approves the budget the CEO asks for (all of it mail); inside the firm,
//! balances flow down the chart: the CEO and the security officers are
//! funded from what the board approves, and every hirer funds their hires.
//! Hiring adds a worker to the run's live chart and boots their VM; letting
//! someone go stops it and hands their work and money to their manager.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use zapper_policy::{valid_worker_name, Firm, Worker, BOARD};

use super::mail::{Draft, Expects, Kind};
use super::{now, FirmRun, WorkerRt, WorkerState};
use crate::daemon::Daemon;

/// Longest budget plan or reason, in bytes.
const MAX_PLAN: usize = 20_000;

#[derive(Debug, Clone, Serialize)]
pub struct BudgetRequest {
    pub id: String,
    pub by: String,
    pub total: u64,
    pub reason: String,
    /// The mail that carries it: the board answers in its thread.
    pub message: String,
    pub thread: String,
    pub created: f64,
    /// `pending`, `approved` or `declined`.
    pub state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided: Option<f64>,
}

/// The run's money: what the board granted, and every worker's balance.
pub struct Ledger {
    /// The run has a budget; without one, nobody's turns are counted against
    /// a balance and nobody hires.
    pub enabled: bool,
    seed: u64,
    share: f64,
    /// The run's budget as the board approved it, seed included; 0 until it
    /// approves one.
    pub approved: u64,
    /// Tokens already paid to the security officers as their share.
    security_paid: u64,
    balance: BTreeMap<String, i64>,
    spent: BTreeMap<String, u64>,
    /// Turns each worker took against their budget.
    turns: BTreeMap<String, u32>,
    received: BTreeMap<String, u64>,
    requests: Vec<BudgetRequest>,
    /// Workers whose manager was told they ran out, until they are funded.
    told: BTreeSet<String>,
}

impl Ledger {
    pub fn new(firm: &Firm) -> Ledger {
        Ledger {
            enabled: firm.budget.seed > 0,
            seed: firm.budget.seed,
            share: firm.budget.security_share,
            approved: 0,
            security_paid: 0,
            balance: BTreeMap::new(),
            spent: BTreeMap::new(),
            turns: BTreeMap::new(),
            received: BTreeMap::new(),
            requests: Vec::new(),
            told: BTreeSet::new(),
        }
    }

    pub fn balance(&self, w: &str) -> i64 {
        self.balance.get(w).copied().unwrap_or(0)
    }

    /// Whether the worker can take a turn: they have tokens, or the run
    /// counts none.
    pub fn can_work(&self, w: &str) -> bool {
        !self.enabled || self.balance(w) > 0
    }

    /// What the run may spend in all: the approved budget, or the seed
    /// before there is one.
    pub fn total(&self) -> u64 {
        self.seed.max(self.approved)
    }

    /// Tokens the worker has spent in turns.
    pub fn spent_by(&self, w: &str) -> u64 {
        self.spent.get(w).copied().unwrap_or(0)
    }

    /// The share of the approved budget spent so far; 0 before the board
    /// approves one.
    pub fn progress(&self) -> f64 {
        if self.approved == 0 {
            return 0.0;
        }
        self.spent.values().sum::<u64>() as f64 / self.approved as f64
    }

    /// Moves `tokens` from `from` (the board if `None`) to `to`.
    pub(super) fn transfer(&mut self, from: Option<&str>, to: &str, tokens: u64) {
        if let Some(f) = from {
            *self.balance.entry(f.to_string()).or_default() -= tokens as i64;
        }
        *self.balance.entry(to.to_string()).or_default() += tokens as i64;
        *self.received.entry(to.to_string()).or_default() += tokens;
        if tokens > 0 {
            self.told.remove(to);
        }
    }

    pub fn spend(&mut self, w: &str, tokens: u64) {
        if !self.enabled {
            return;
        }
        *self.balance.entry(w.to_string()).or_default() -= tokens as i64;
        *self.spent.entry(w.to_string()).or_default() += tokens;
        *self.turns.entry(w.to_string()).or_default() += 1;
    }

    /// What turns have cost so far, by role: workers, turns, tokens, and
    /// tokens per turn on average. Prices differ by model, so the CEO sizes
    /// the budget from this, not from fixed numbers.
    pub fn observed(&self, chart: &Firm, models: &BTreeMap<String, Option<String>>) -> Vec<Value> {
        let mut by: BTreeMap<&str, (u32, u32, u64)> = BTreeMap::new();
        for w in &chart.workers {
            let turns = self.turns.get(&w.name).copied().unwrap_or(0);
            let tokens = self.spent.get(&w.name).copied().unwrap_or(0);
            let e = by.entry(w.role.as_str()).or_default();
            e.0 += 1;
            e.1 += turns;
            e.2 += tokens;
        }
        by.into_iter()
            .filter(|(_, (_, turns, _))| *turns > 0)
            .map(|(role, (workers, turns, tokens))| {
                json!({ "role": role, "model": models.get(role).cloned().flatten(), "workers": workers, "turns": turns, "tokens": tokens, "tokens_per_turn": tokens / turns as u64 })
            })
            .collect()
    }

    fn pending(&self) -> Option<&BudgetRequest> {
        self.requests.iter().find(|r| r.state == "pending")
    }

    pub fn status(&self) -> Value {
        let mut names: BTreeSet<&String> = self.balance.keys().collect();
        names.extend(self.spent.keys());
        let workers: BTreeMap<&String, Value> = names
            .into_iter()
            .map(|n| (n, json!({ "balance": self.balance(n), "spent": self.spent.get(n).copied().unwrap_or(0), "received": self.received.get(n).copied().unwrap_or(0) })))
            .collect();
        let spent: u64 = self.spent.values().sum();
        json!({
            "enabled": self.enabled, "seed": self.seed, "approved": self.approved, "security_share": self.share,
            "total": self.total(), "spent": spent, "workers": workers, "requests": self.requests,
        })
    }
}

pub(super) fn fmt_tokens(n: i64) -> String {
    let a = n.unsigned_abs() as f64;
    let s = if a >= 1e6 {
        format!("{:.1}M", a / 1e6)
    } else if a >= 1e3 {
        format!("{:.0}k", a / 1e3)
    } else {
        format!("{a}")
    };
    if n < 0 {
        format!("-{s}")
    } else {
        s
    }
}

/// A token amount from a tool argument: a number, or text like `2M`.
pub fn tokens_arg(v: &Value, name: &str) -> Result<i64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| anyhow!("{name}: a whole number of tokens")),
        Value::String(s) => {
            let neg = s.trim().starts_with('-');
            let n = zapper_policy::parse_tokens(s.trim().trim_start_matches('-'))? as i64;
            Ok(if neg { -n } else { n })
        }
        Value::Null => bail!("{name} is required: tokens, like 2M or 500k"),
        _ => bail!("{name}: tokens, like 2M or 500k"),
    }
}

/// Splits `tokens` from the board: the security officers' share among the
/// officers, the rest to the CEO (the chart's root), who keeps the share
/// while they hold security themselves.
fn grant(run: &FirmRun, tokens: u64, reason: &str) {
    let chart = run.chart();
    let Some(root) = chart.root().map(str::to_string) else {
        return;
    };
    let officers: Vec<String> = chart
        .security
        .officers
        .iter()
        .filter(|o| chart.worker(o).is_some() && **o != root)
        .cloned()
        .collect();
    let mut l = run.ledger.lock().unwrap();
    let security = if officers.is_empty() {
        0
    } else {
        (tokens as f64 * l.share).round() as u64
    };
    let each = security / officers.len().max(1) as u64;
    let mut moves = Vec::new();
    for o in &officers {
        l.transfer(None, o, each);
        moves.push((o.clone(), each));
    }
    l.security_paid += each * officers.len() as u64;
    let rest = tokens - each * officers.len() as u64;
    l.transfer(None, &root, rest);
    moves.push((root, rest));
    drop(l);
    for (to, t) in moves {
        run.journal.append(
            "funds",
            json!({ "event": "move", "from": BOARD, "to": to, "tokens": t, "reason": reason }),
        );
    }
}

/// Security is funded by the board: when officers take security over from
/// the CEO, the share of the budget the CEO kept for them goes to them
/// (what the CEO has of it).
pub fn settle_security(run: &FirmRun) {
    let chart = run.chart();
    let Some(root) = chart.root().map(str::to_string) else {
        return;
    };
    let officers: Vec<String> = chart
        .security
        .officers
        .iter()
        .filter(|o| **o != root && chart.worker(o).is_some())
        .cloned()
        .collect();
    if officers.is_empty() {
        return;
    }
    let mut l = run.ledger.lock().unwrap();
    if !l.enabled {
        return;
    }
    let owed = ((l.total() as f64 * l.share).round() as u64).saturating_sub(l.security_paid);
    let give = owed.min(l.balance(&root).max(0) as u64);
    let each = give / officers.len() as u64;
    if each == 0 {
        return;
    }
    for o in &officers {
        l.transfer(Some(&root), o, each);
    }
    l.security_paid += each * officers.len() as u64;
    drop(l);
    for o in &officers {
        run.journal.append(
            "funds",
            json!({ "event": "move", "from": root, "to": o, "tokens": each, "reason": "security's share of the approved budget, handed over with security" }),
        );
        if let Some(w) = run.rt(o) {
            w.wake.notify_one();
        }
    }
}

/// The seed, at the start of a run with a budget.
pub fn seed(run: &FirmRun) {
    let (enabled, seed) = {
        let l = run.ledger.lock().unwrap();
        (l.enabled, l.seed)
    };
    if enabled {
        grant(run, seed, "seed: to scope the board's task");
    }
}

/// A line for the wake prompt: the worker's balance, and for a manager
/// their team's.
pub fn balance_note(run: &FirmRun, w: &str) -> Option<String> {
    let l = run.ledger.lock().unwrap();
    if !l.enabled {
        return None;
    }
    let mut s = format!(
        "Your budget: {} tokens left (spent {}).",
        fmt_tokens(l.balance(w)),
        fmt_tokens(l.spent.get(w).copied().unwrap_or(0) as i64)
    );
    let reports = run
        .chart()
        .reports_of(w)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !reports.is_empty() {
        let team: Vec<String> = reports
            .iter()
            .map(|r| format!("{r} {}", fmt_tokens(l.balance(r))))
            .collect();
        s.push_str(&format!(" Your reports' balances: {}.", team.join(", ")));
    }
    if run.chart().root() == Some(w) && l.approved == 0 {
        s.push_str(" No budget is approved yet: scope the board's task on the seed, then ask with budget_request.");
    }
    Some(s)
}

/// Your balance and your team's, for the `budget` tool.
pub fn summary(run: &FirmRun, w: &str) -> Value {
    let chart = run.chart();
    let l = run.ledger.lock().unwrap();
    let team: Vec<Value> = chart
        .workers
        .iter()
        .filter(|x| chart.is_above(w, &x.name))
        .map(|x| json!({ "worker": x.name, "title": chart.title_of(&x.name), "reports_to": x.reports_to, "balance": l.balance(&x.name), "spent": l.spent.get(&x.name).copied().unwrap_or(0) }))
        .collect();
    let mine = json!({ "balance": l.balance(w), "spent": l.spent.get(w).copied().unwrap_or(0), "received": l.received.get(w).copied().unwrap_or(0) });
    let firm = (chart.root() == Some(w))
        .then(|| json!({ "approved": l.approved, "seed": l.seed, "requests": l.requests, "observed_cost": l.observed(&chart, &run.models) }));
    json!({ "enabled": l.enabled, "you": mine, "team": team, "firm": firm })
}

/// Whether `w` has money for a turn. Tells their manager (the board, for
/// the CEO and the founding officers) once when they run out.
pub fn allows(run: &FirmRun, w: &str) -> bool {
    let mut l = run.ledger.lock().unwrap();
    if !l.enabled || l.balance(w) > 0 {
        return true;
    }
    if !l.told.insert(w.to_string()) {
        return false;
    }
    let spent = l.spent.get(w).copied().unwrap_or(0) as i64;
    drop(l);
    let chart = run.chart();
    let founding_officer =
        chart.is_officer(w) && chart.root() != Some(w) && run.config.worker(w).is_some();
    let manager = chart
        .manager_of(w)
        .filter(|_| !founding_officer)
        .map(str::to_string);
    run.journal
        .append("funds", json!({ "event": "exhausted", "worker": w }));
    // An officer who can't work no longer takes requests: they go to the
    // others, or to the board.
    if chart.is_officer(w) && chart.root() != Some(w) {
        super::security::requeue(run, w);
    }
    match manager {
        Some(m) if m == BOARD => run.notice(
            BOARD,
            &format!("{w} ran out of budget"),
            &format!("[Automatic notice] {w}, {}, has spent their budget ({} tokens) and stopped taking turns{}. Fund them with `zapper firm fund {} {w} TOKENS --reason …`.\n", chart.title_of(w), fmt_tokens(spent), if chart.is_officer(w) { "; their pending access requests and background checks went to the other officers or to you in the Security tab" } else { "" }, run.firm),
        ),
        Some(m) => run.notice(
            &m,
            &format!("{w} ran out of budget"),
            &format!("[Automatic notice] {w} has spent their budget ({} tokens) and stopped taking turns; their mail waits. Fund them with fund(worker: \"{w}\", tokens: …, reason: …) from your own balance, or reassign their work.\n", fmt_tokens(spent)),
        ),
        None => run.notice(
            BOARD,
            &format!("{w} ran out of budget"),
            &format!("[Automatic notice] {w} has spent their budget ({} tokens) and stopped taking turns. {}\n", fmt_tokens(spent), if founding_officer { "Security officers are funded by their share of each budget the board approves." } else { "The CEO can ask the board for more with budget_request." }),
        ),
    }
    false
}

/// The CEO asks the board for a budget: one number, a plan and a reason, in
/// a mail to the board, which answers in its thread.
pub fn request(run: &FirmRun, by: &str, total: i64, plan: &str, reason: &str) -> Result<Value> {
    let chart = run.chart();
    if chart.root() != Some(by) {
        bail!(
            "only the CEO ({}) asks the board for a budget; ask your manager",
            chart.root().unwrap_or("the root")
        );
    }
    if !run.ledger.lock().unwrap().enabled {
        bail!("this firm has no run budget (budget.seed in the config)");
    }
    if total <= 0 {
        bail!("total: how many tokens, like 40M");
    }
    if plan.trim().is_empty() || plan.len() > MAX_PLAN || reason.len() > MAX_PLAN {
        bail!("give a plan (who you will hire, what you will contract, a reserve) of up to {MAX_PLAN} bytes");
    }
    {
        let l = run.ledger.lock().unwrap();
        if total as u64 <= l.total() {
            bail!(
                "the budget is the run's total, seed included: you have {} already, so ask for more than that",
                fmt_tokens(l.total() as i64)
            );
        }
    }
    if let Some(p) = run.ledger.lock().unwrap().pending() {
        bail!(
            "request {} ({} tokens) is waiting for the board; wait for its answer",
            p.id,
            fmt_tokens(p.total as i64)
        );
    }
    let id = format!("b-{}", run.ledger.lock().unwrap().requests.len() + 1);
    let body = format!("**Budget request {id}: {} tokens in all, seed included.**\n\n{}\n\n**Why:** {}\n\nApprove or decline it in the Kickoff tab; a decline with a note sends it back.\n", fmt_tokens(total), plan.trim(), reason.trim());
    let mut d = Draft::new(
        by,
        vec![BOARD.into()],
        &format!("Budget request {id}: {} tokens", fmt_tokens(total)),
        &body,
        Kind::Question,
    );
    d.expects = Some(Expects::Reply);
    let sent = run.send(d)?;
    let r = BudgetRequest {
        id: id.clone(),
        by: by.into(),
        total: total as u64,
        reason: reason.trim().into(),
        message: sent.id.clone(),
        thread: sent.thread.clone(),
        created: now(),
        state: "pending".into(),
        note: String::new(),
        decided: None,
    };
    run.journal.append("funds", json!({ "event": "request", "id": id, "by": by, "total": total, "plan": plan.trim(), "reason": reason.trim(), "message": sent.id, "thread": sent.thread }));
    run.ledger.lock().unwrap().requests.push(r);
    Ok(
        json!({ "request": id, "message": sent.id, "note": "the board will answer in the thread; end your turn" }),
    )
}

/// The board approves or declines a budget request, answering in its thread.
pub fn decide(run: &FirmRun, id: &str, approve: bool, note: &str) -> Result<Value> {
    let (r, granted) = {
        let mut l = run.ledger.lock().unwrap();
        let r = l
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow!("no budget request {id}"))?;
        if r.state != "pending" {
            bail!("budget request {id} was already {}", r.state);
        }
        r.state = if approve { "approved" } else { "declined" }.into();
        r.note = note.trim().chars().take(4000).collect();
        r.decided = Some(now());
        let r = r.clone();
        // The approved budget includes what the seed already gave.
        let new = if approve {
            let new = r.total.saturating_sub(l.total());
            l.approved = l.approved.max(r.total);
            new
        } else {
            0
        };
        (r, new)
    };
    run.journal.append(
        "funds",
        json!({ "event": "decided", "id": id, "approved": approve, "note": r.note, "granted": granted }),
    );
    if granted > 0 {
        grant(run, granted, &format!("budget {id}, approved by the board"));
    }
    let balance = run.ledger.lock().unwrap().balance(&r.by);
    let body = if approve {
        format!(
            "**Approved: {} tokens in all** (budget {id}), the seed included: {} new.{}\n\nYour balance is now {} tokens; the security officers have their share. Hire and contract from it: running the firm is up to you.\n",
            fmt_tokens(r.total as i64),
            fmt_tokens(granted as i64),
            if r.note.is_empty() { String::new() } else { format!("\n\n{}", r.note) },
            fmt_tokens(balance)
        )
    } else {
        format!("**Declined** (budget {id}).\n\n{}\n\nRevise the plan or the number and ask again with budget_request.\n", if r.note.is_empty() { "No note." } else { r.note.as_str() })
    };
    let mut d = Draft::new(BOARD, vec![r.by.clone()], "", &body, Kind::Reply);
    d.in_reply_to = Some(r.message.clone());
    let sent = run.send(d)?;
    Ok(json!({ "request": r, "message": sent.id }))
}

/// The board gives a worker tokens from nothing, on top of the budget it
/// approved (which grows by that much).
fn board_fund(run: &FirmRun, worker: &str, tokens: u64, reason: &str) -> Result<Value> {
    if run.chart().worker(worker).is_none() {
        bail!("no worker {worker}");
    }
    if reason.trim().is_empty() {
        bail!("give a reason");
    }
    {
        let mut l = run.ledger.lock().unwrap();
        if !l.enabled {
            bail!("this firm has no run budget");
        }
        if l.approved == 0 {
            l.approved = l.seed;
        }
        l.approved += tokens;
        l.transfer(None, worker, tokens);
    }
    run.journal.append(
        "funds",
        json!({ "event": "move", "from": BOARD, "to": worker, "tokens": tokens, "reason": reason.trim(), "granted": tokens }),
    );
    if let Some(w) = run.rt(worker) {
        w.wake.notify_one();
    }
    Ok(json!({ "worker": worker, "their_balance": run.ledger.lock().unwrap().balance(worker) }))
}

/// A manager moves tokens to a report (or back, with a negative amount).
pub fn fund(run: &FirmRun, by: &str, worker: &str, tokens: i64, reason: &str) -> Result<Value> {
    if by == BOARD && tokens > 0 {
        return board_fund(run, worker, tokens as u64, reason);
    }
    let chart = run.chart();
    if chart.worker(worker).is_none() {
        bail!("no worker {worker}");
    }
    if by != BOARD && !chart.is_above(by, worker) {
        bail!("only {worker}'s managers fund them");
    }
    if tokens == 0 {
        bail!("tokens: how many to give (or, negative, to take back)");
    }
    if reason.trim().is_empty() {
        bail!("give a reason");
    }
    let mut l = run.ledger.lock().unwrap();
    if !l.enabled {
        bail!("this firm has no run budget");
    }
    if tokens > 0 {
        if l.balance(by) < tokens {
            bail!(
                "you have {} tokens; you can't give {}",
                fmt_tokens(l.balance(by)),
                fmt_tokens(tokens)
            );
        }
        l.transfer(Some(by), worker, tokens as u64);
    } else {
        let back = -tokens;
        if l.balance(worker) < back {
            bail!(
                "{worker} has {} tokens left; you can take back at most that",
                fmt_tokens(l.balance(worker).max(0))
            );
        }
        l.transfer(Some(worker), by, back as u64);
    }
    let (mine, theirs) = (l.balance(by), l.balance(worker));
    drop(l);
    let (from, to, t) = if tokens > 0 {
        (by, worker, tokens)
    } else {
        (worker, by, -tokens)
    };
    run.journal.append(
        "funds",
        json!({ "event": "move", "from": from, "to": to, "tokens": t, "reason": reason.trim() }),
    );
    if let Some(w) = run.rt(worker) {
        w.wake.notify_one();
    }
    Ok(json!({ "worker": worker, "their_balance": theirs, "your_balance": mine }))
}

/// What a role may reach, for the officers' background check: its
/// profile's network and exec rules, and the firm's credentials it gets.
pub(super) fn access_summary(run: &FirmRun, config: &zapper_policy::Config, role: &str) -> String {
    let Some(r) = run.config.roles.get(role) else {
        return String::new();
    };
    let mut lines = vec![format!("profile {}", r.profile)];
    if let Ok(p) = config.resolve(&r.profile) {
        let mut net = Vec::new();
        let mut exec = Vec::new();
        for (_, layer) in &p.layers {
            for n in &layer.net {
                let ports = if n.ports.is_empty() {
                    String::new()
                } else {
                    format!(
                        ":{}",
                        n.ports
                            .iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                };
                net.push(
                    format!(
                        "{:?} {}{}{}",
                        n.action,
                        n.host,
                        ports,
                        if n.secrets.is_empty() {
                            String::new()
                        } else {
                            format!(" (with secrets {})", n.secrets.join(", "))
                        }
                    )
                    .to_lowercase(),
                );
            }
            for e in &layer.exec {
                exec.push(format!("{:?} {}", e.action, e.path).to_lowercase());
            }
            if let Some(dn) = layer.defaults.net {
                net.push(format!("otherwise {:?}", dn).to_lowercase());
            }
        }
        if !net.is_empty() {
            lines.push(format!("network: {}", net.join("; ")));
        }
        if !exec.is_empty() {
            lines.push(format!("exec: {}", exec.join("; ")));
        }
    }
    lines.push("credentials: none at the start; git, the cluster and drive spaces come only with grants the policy owners decide".into());
    lines.join("\n")
}

/// A free name for a hire: the hirer's choice, or the next from the pool.
fn pick_name(run: &FirmRun, chart: &Firm, role: &str, wanted: Option<&str>) -> Result<String> {
    let departed = run.departed.lock().unwrap().clone();
    let taken = |n: &str| {
        chart.worker(n).is_some()
            || departed.contains_key(n)
            || run.config.facilities.contains_key(n)
            || n == super::facility::CLUSTER
    };
    if let Some(n) = wanted {
        let n = n.trim().to_ascii_lowercase();
        if !valid_worker_name(&n) || zapper_policy::reserved_name(&n) {
            bail!("{n:?} can't be a name: lowercase letters, digits and dashes, starting with a letter");
        }
        if taken(&n) {
            bail!("{n} is taken in this firm; pick another name");
        }
        return Ok(n);
    }
    if let Some(n) = run.config.names.iter().find(|n| !taken(n)) {
        return Ok(n.clone());
    }
    // No pool in the config: a random free name from the built-in one.
    use rand::seq::SliceRandom;
    let mut pool: Vec<&str> = NAMES.iter().copied().filter(|n| !taken(n)).collect();
    pool.shuffle(&mut rand::thread_rng());
    if let Some(n) = pool.first() {
        return Ok(n.to_string());
    }
    (1..1000)
        .map(|i| format!("{role}-{i}"))
        .find(|n| !taken(n))
        .ok_or_else(|| anyhow!("no free name"))
}

/// Names for hires whose hirer doesn't pick one, when the config has no pool.
const NAMES: &[&str] = &[
    "ada",
    "alan",
    "anders",
    "annie",
    "barbara",
    "bjarne",
    "claude",
    "dennis",
    "donald",
    "edsger",
    "ellen",
    "frances",
    "grace",
    "guido",
    "hedy",
    "iris",
    "joan",
    "john",
    "katherine",
    "ken",
    "linus",
    "margaret",
    "mary",
    "niklaus",
    "radia",
    "sophie",
    "tim",
    "vint",
];

pub struct HireArgs {
    pub role: String,
    pub name: Option<String>,
    pub title: Option<String>,
    pub budget: i64,
    pub subject: String,
    pub brief: String,
    pub reason: String,
    pub due: Option<f64>,
    pub attachments: Vec<String>,
}

/// Hires a worker under `by`: checks the role, the money and the host,
/// adds them to the chart, funds them, boots their VM, and sends them the
/// brief as their first assignment.
pub async fn hire(d: &Arc<Daemon>, run: &Arc<FirmRun>, by: &str, a: HireArgs) -> Result<Value> {
    let hirer = run.worker(by)?;
    let can = run
        .config
        .roles
        .get(&hirer.role)
        .map(|r| r.hires.clone())
        .unwrap_or_default();
    super::security::officer_first(run, by, &a.role)?;
    if !can.contains(&a.role) {
        if can.is_empty() {
            bail!("your role ({}) doesn't hire; ask your manager", hirer.role);
        }
        bail!("you can hire {}; not {}", can.join(", "), a.role);
    }
    let role = run
        .config
        .roles
        .get(&a.role)
        .cloned()
        .ok_or_else(|| anyhow!("no role {}", a.role))?;
    {
        let l = run.ledger.lock().unwrap();
        if !l.enabled {
            bail!("this firm has no run budget, so it doesn't hire");
        }
        if l.approved == 0 {
            bail!("the board hasn't approved a budget yet: scope the task and ask with budget_request first");
        }
        if a.budget <= 0 {
            bail!("budget: the tokens you give them for the run, like 5M");
        }
        if l.balance(by) < a.budget {
            bail!(
                "you have {} tokens; a {} budget is more than that",
                fmt_tokens(l.balance(by)),
                fmt_tokens(a.budget)
            );
        }
    }
    if a.brief.trim().is_empty() || a.reason.trim().is_empty() {
        bail!("give a brief (their first assignment: the job, context, what done looks like) and a reason");
    }
    if a.brief.len() > super::mail::MAX_BODY || a.reason.len() > MAX_PLAN {
        bail!("the brief or the reason is too long");
    }
    let config = room(d, &a.role, &role)?;
    let (name, firm, rt) = join(
        run,
        by,
        &a.role,
        a.name.as_deref(),
        a.title.clone(),
        a.budget,
        a.reason.trim(),
        None,
    )?;
    // Training first: how mail and credentials work, before the brief.
    super::prompts::onboard(run, &name);
    // The brief: their first assignment, waiting for them when they boot.
    // An officer's brief is a standing role, from someone who isn't their
    // manager: not an assignment.
    let mut draft = Draft::new(
        by,
        vec![name.clone()],
        &a.subject,
        &a.brief,
        if role.officer {
            Kind::Fyi
        } else {
            Kind::Assignment
        },
    );
    draft.due = a.due;
    draft.attachments = a.attachments.clone();
    draft.context = hirer.context.lock().unwrap().clone();
    // An officer's brief is a standing role, never done: nobody waits on it.
    if role.officer {
        draft.expects = Some(Expects::Nothing);
        draft.due = None;
    }
    let sent = run.send(draft).context("sending the brief")?;
    // A background check by the security officers before they start: their
    // VM boots meanwhile, and they take no turns until they are cleared.
    let access = access_summary(run, &config, &a.role);
    let check = super::security::screen(
        run,
        &name,
        by,
        &a.role,
        &firm.title_of(&name),
        a.budget,
        a.reason.trim(),
        a.brief.trim(),
        &access,
    );
    // Their VM, then their foreman: the run's booter does both.
    run.set_state(&rt, WorkerState::Booting, None);
    let _ = run.boots.send(rt);
    // The board has no reason to hold back the firm's first security
    // officer: its own check clears them at once.
    if role.officer && super::security::is_first_officer(run, &name) {
        let _ = super::security::decide_screening(
            d,
            run,
            BOARD,
            &check,
            true,
            "The board clears the firm's first security officer without being asked.",
        );
    }
    let left = run.ledger.lock().unwrap().balance(by);
    Ok(json!({
        "worker": name, "title": firm.title_of(&name), "reports_to": by, "budget": a.budget, "your_balance": left,
        "brief": sent.id, "thread": sent.thread, "background_check": check,
        "note": "their VM is booting and the security officers check them; once cleared they get the brief, and their report will come to you",
    }))
}

/// Room on the host for one more VM of `role`: the config, or why not.
pub(super) fn room(
    d: &Arc<Daemon>,
    role_name: &str,
    role: &zapper_policy::Role,
) -> Result<zapper_policy::Config> {
    let config = d.config.read().unwrap().clone();
    let profile = config.resolve(&role.profile)?;
    if let Some(avail) = super::mem_available_mib() {
        if profile.engine.memory_mib as u64 > avail {
            bail!("the host has {avail} MiB of memory available, and a {role_name} needs {} MiB; tell the board", profile.engine.memory_mib);
        }
    }
    let running = d.sessions.read().unwrap().len();
    if running >= config.daemon.max_sessions {
        bail!("the host runs {running} sessions, its limit; tell the board");
    }
    Ok(config)
}

/// Someone joins the firm under `by`: into the chart, funded from `by`'s
/// balance, with their credentials; `contract` for a contractor. Their
/// name, the chart with them, and their runtime (not booted yet).
#[allow(clippy::too_many_arguments)]
pub(super) fn join(
    run: &FirmRun,
    by: &str,
    role: &str,
    wanted: Option<&str>,
    title: Option<String>,
    budget: i64,
    reason: &str,
    contract: Option<&str>,
) -> Result<(String, Firm, Arc<WorkerRt>)> {
    let chart = run.chart();
    let name = pick_name(run, &chart, role, wanted)?;
    // Security officers answer to the board, not to whoever hired them.
    let officer = run.config.roles.get(role).is_some_and(|r| r.officer);
    let w = Worker {
        name: name.clone(),
        role: role.to_string(),
        reports_to: Some(if officer { BOARD } else { by }.to_string()),
        title: title
            .map(|t| t.trim().chars().take(80).collect())
            .filter(|t: &String| !t.is_empty()),
    };
    {
        let mut c = run.chart.write().unwrap();
        if c.worker(&name).is_some() {
            bail!("{name} is taken in this firm; pick another name");
        }
        c.workers.push(w.clone());
    }
    let firm = run.chart();
    // A scenario's slot for this hire gives them the board's instructions.
    super::scenario::claim(run, role, contract.is_some(), &name);
    let rt = Arc::new(WorkerRt::new(&firm, &w));
    run.workers
        .write()
        .unwrap()
        .insert(name.clone(), rt.clone());
    run.ledger
        .lock()
        .unwrap()
        .transfer(Some(by), &name, budget as u64);
    let mut worker = super::worker_json(&firm, &run.models, &w);
    if let Some(c) = contract {
        worker["contract"] = json!(c);
    }
    run.journal.append("hired", json!({ "worker": worker, "by": by, "budget": budget, "reason": reason, "contract": contract }));
    let why = match contract {
        Some(c) => format!("quoting fee for contract {c}"),
        None => format!("hired as {}", firm.title_of(&name)),
    };
    run.journal.append(
        "funds",
        json!({ "event": "move", "from": by, "to": name, "tokens": budget, "reason": why }),
    );
    // Their git identity; their grant decides what it may do.
    if run.config.git.is_some() {
        run.facility
            .git_passwords
            .lock()
            .unwrap()
            .insert(name.clone(), crate::daemon::random_id(24));
    }
    Ok((name, firm, rt))
}

/// Lets a worker go: their VM stops, their reports and open work move to
/// their manager, and so does their unspent budget.
pub async fn release(
    d: &Arc<Daemon>,
    run: &Arc<FirmRun>,
    by: &str,
    worker: &str,
    reason: &str,
) -> Result<Value> {
    let chart = run.chart();
    if chart.worker(worker).is_none() {
        bail!("no worker {worker}");
    }
    if by != BOARD && chart.manager_of(worker) == Some(BOARD) {
        bail!(
            "{worker} is a security officer and answers to the board; only the board lets them go"
        );
    }
    if by != BOARD && !chart.is_above(by, worker) {
        bail!("only {worker}'s managers can let them go");
    }
    if chart.root() == Some(worker) || (by != BOARD && run.config.worker(worker).is_some()) {
        bail!("{worker} was appointed by the board; only the board changes that");
    }
    if reason.trim().is_empty() {
        bail!("give a reason");
    }
    if super::scenario::begin_debrief(run, worker) {
        super::scenario::debrief(run, worker, reason.trim()).await;
    }
    Ok(let_go(d, run, by, worker, reason, true))
}

/// A worker leaves the firm: out of the chart, their VM stopped; their
/// reports, open work, mail and unspent budget go to their manager, who is
/// told when someone else (`by`) made the call and `tell` is set.
pub fn let_go(
    d: &Arc<Daemon>,
    run: &Arc<FirmRun>,
    by: &str,
    worker: &str,
    reason: &str,
    tell: bool,
) -> Value {
    let chart = run.chart();
    let manager = chart.manager_of(worker).unwrap_or(BOARD).to_string();
    {
        let mut c = run.chart.write().unwrap();
        c.workers.retain(|w| w.name != worker);
        for w in c.workers.iter_mut() {
            if w.reports_to.as_deref() == Some(worker) {
                w.reports_to = Some(manager.clone());
            }
        }
    }
    super::security::officer_left(run, worker, by);
    // What they had to decide (as the owner of the firm's services) moves on.
    super::security::requeue(run, worker);
    super::contracts::left(run, worker, reason);
    // Offboarding: every grant goes.
    super::access::revoke_all(
        d,
        run,
        worker,
        by,
        &format!("left the firm: {}", reason.trim()),
    );
    run.departed
        .lock()
        .unwrap()
        .insert(worker.to_string(), manager.clone());
    let (moved, to_read) = run.post.lock().unwrap().leave(worker, &manager);
    // What they hadn't read or answered reaches their manager's desk.
    for id in to_read {
        run.deliver(&super::mail::Sent {
            id,
            thread: String::new(),
            recipients: vec![manager.clone()],
            objective_done: false,
        });
    }
    let returned = {
        let mut l = run.ledger.lock().unwrap();
        let left = l.balance(worker).max(0) as u64;
        if left > 0 {
            l.transfer(Some(worker), &manager, left);
        }
        left
    };
    let rt = run.workers.write().unwrap().remove(worker);
    run.journal.append("left", json!({ "worker": worker, "by": by, "reason": reason.trim(), "to": manager, "returned": returned, "moved": moved.iter().map(|o| &o.message).collect::<Vec<_>>() }));
    if returned > 0 {
        run.journal.append("funds", json!({ "event": "move", "from": worker, "to": manager, "tokens": returned, "reason": format!("{worker} left") }));
    }
    if let Some(rt) = rt {
        run.set_state(&rt, WorkerState::Off, None);
        rt.wake.notify_one();
        let s = rt.session.lock().unwrap().take();
        if let Some(s) = s {
            let d = d.clone();
            tokio::spawn(async move {
                crate::daemon::session::stop(&d, &s, "let go").await;
                d.sessions.write().unwrap().remove(&s.id);
            });
        }
    }
    if tell && manager != by && manager != BOARD {
        let list: String = moved
            .iter()
            .map(|o| format!("- \"{}\" from {} ({})\n", o.subject, o.creditor, o.thread))
            .collect();
        run.notice(
            &manager,
            &format!("{worker} left; their work is yours"),
            &format!("[Automatic notice] {by} let {worker} go: {}\nTheir reports now report to you, their mail comes to you (what they hadn't read, and the requests they hadn't answered, are in your inbox now), and you have their unspent budget ({} tokens).\n{}", reason.trim(), fmt_tokens(returned as i64), if list.is_empty() { String::new() } else { format!("What they owed is now yours:\n{list}") }),
        );
    }
    json!({ "worker": worker, "returned": returned, "to": manager, "moved": moved.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_moves_and_spends() {
        let mut f = Firm::default();
        f.budget.seed = 1000;
        let mut l = Ledger::new(&f);
        assert!(l.enabled);
        l.transfer(None, "ceo", 1000);
        l.transfer(Some("ceo"), "cto", 400);
        l.spend("cto", 450);
        assert_eq!(l.balance("ceo"), 600);
        assert_eq!(l.balance("cto"), -50);
        let s = l.status();
        assert_eq!(s["spent"], 450);
        assert_eq!(s["workers"]["cto"]["received"], 400);
        assert_eq!(tokens_arg(&json!("2M"), "t").unwrap(), 2_000_000);
        assert_eq!(tokens_arg(&json!("-500k"), "t").unwrap(), -500_000);
        assert_eq!(tokens_arg(&json!(1200), "t").unwrap(), 1200);
        assert_eq!(fmt_tokens(40_000_000), "40.0M");
        assert_eq!(fmt_tokens(-1500), "-2k");
    }

    #[test]
    fn observed_cost_by_role() {
        let c = zapper_policy::Config::from_yaml(
            "profiles: { w: {} }\nfirms:\n  acme:\n    budget: { seed: 1000 }\n    roles: { boss: { profile: w, skills: { work: ceo } }, eng: { profile: w, skills: { work: x } } }\n    workers:\n      - { name: ceo, role: boss }\n      - { name: ada, role: eng, reports_to: ceo }\n      - { name: bo, role: eng, reports_to: ceo }\n",
        )
        .unwrap();
        let f = &c.firms["acme"];
        let mut l = Ledger::new(f);
        l.spend("ceo", 800);
        l.spend("ada", 3000);
        l.spend("ada", 1000);
        l.spend("bo", 2000);
        let models = BTreeMap::from([("eng".to_string(), Some("m-big".to_string()))]);
        let o = l.observed(f, &models);
        let eng = o.iter().find(|x| x["role"] == "eng").unwrap();
        assert_eq!(
            (
                eng["workers"].clone(),
                eng["turns"].clone(),
                eng["tokens_per_turn"].clone()
            ),
            (json!(2), json!(3), json!(2000))
        );
        assert_eq!(eng["model"], "m-big");
        assert_eq!(
            o.iter().find(|x| x["role"] == "boss").unwrap()["tokens_per_turn"],
            800
        );
    }
}
