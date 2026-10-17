//! The office: firms of workers, each a Claude Code in its own session, who
//! talk by mail, publish to a shared drive, and whose network access security
//! officers decide. `docs/office.md` is the design.
//!
//! A run starts fresh with every `firm up` and is written to
//! `state/firms/<firm>/runs/<id>/` as it happens; the daemon keeps indexes.

pub mod drive;
pub mod facility;
pub mod foreman;
pub mod git;
pub mod http;
pub mod journal;
pub mod mail;
pub mod ops;
pub mod prompts;
pub mod security;
pub mod skills;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{Notify, Semaphore};
use zapper_policy::proto::GuestFile;
use zapper_policy::{Config, Firm, Mount, MountMode, Policy, BOARD};

use super::session::{self, NewSession, Session};
use super::{now, random_id, Daemon};
use journal::Journal;
use mail::{Draft, Kind, PostOffice};

/// Every firm's current or last run.
#[derive(Default)]
pub struct Office {
    pub runs: RwLock<BTreeMap<String, Arc<FirmRun>>>,
}

impl Office {
    pub fn run(&self, firm: &str) -> Result<Arc<FirmRun>> {
        self.runs.read().unwrap().get(firm).cloned().ok_or_else(|| anyhow!("no run of firm {firm}; start it with `zapper firm up {firm}`"))
    }

    pub fn active(&self, firm: &str) -> Result<Arc<FirmRun>> {
        let r = self.run(firm)?;
        if r.ended() {
            bail!("firm {firm} isn't running; start it with `zapper firm up {firm}`");
        }
        Ok(r)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Starting,
    Running,
    Paused,
    Stopping,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerState {
    Off,
    Booting,
    Idle,
    Waiting,
    Queued,
    Working,
    Reviewing,
    OverBudget,
    Error,
}

/// A worker of a running firm.
pub struct WorkerRt {
    pub name: String,
    pub role: String,
    pub title: String,
    pub officer: bool,
    pub session: Mutex<Option<Arc<Session>>>,
    pub state: Mutex<(WorkerState, Option<String>)>,
    /// Messages not yet shown to the worker.
    pub queue: Mutex<Vec<String>>,
    /// Texts to put before the next wake prompt.
    pub notes: Mutex<Vec<String>>,
    /// Access requests waiting for this officer.
    pub access_queue: Mutex<Vec<String>>,
    /// The officer has a traffic digest to read.
    pub digest_due: AtomicBool,
    pub wake: Notify,
    /// The thread the current turn is about, for new threads it starts.
    pub context: Mutex<Option<String>>,
}

impl WorkerRt {
    pub fn state(&self) -> WorkerState {
        self.state.lock().unwrap().0
    }

    pub fn session(&self) -> Option<Arc<Session>> {
        self.session.lock().unwrap().clone()
    }
}

/// Turn and token use, reset at midnight UTC.
#[derive(Debug, Default, Serialize)]
pub struct Usage {
    pub day: i64,
    pub turns: u32,
    pub tokens: u64,
    pub worker_turns: HashMap<String, u32>,
    pub notified: bool,
}

pub struct FirmRun {
    pub firm: String,
    pub id: String,
    pub dir: PathBuf,
    pub config: Firm,
    pub domain: String,
    pub started: f64,
    pub state: Mutex<RunState>,
    pub journal: Journal,
    pub post: Mutex<PostOffice>,
    pub workers: BTreeMap<String, Arc<WorkerRt>>,
    pub drive: Mutex<drive::Drive>,
    pub security: Mutex<security::Security>,
    /// Officers' standing approvals per role.
    pub roles: BTreeMap<String, Arc<RwLock<Policy>>>,
    pub turns: Arc<Semaphore>,
    pub usage: Mutex<Usage>,
    pub facility: facility::Facilities,
    pub stopping: Notify,
}

/// Links a session to the firm it belongs to.
#[derive(Clone)]
pub struct FirmLink {
    pub firm: String,
    /// A worker's or facility's name.
    pub member: String,
    pub worker: bool,
    pub run: Weak<FirmRun>,
    pub role: Option<(String, Arc<RwLock<Policy>>)>,
}

impl FirmLink {
    pub fn run(&self) -> Option<Arc<FirmRun>> {
        self.run.upgrade()
    }
}

/// `20261016-140501-k3z`, sortable by time.
fn run_id() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}-{}", t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second(), random_id(3))
}

/// Whether `s` has the form `run_id` makes, so it can go into a path.
pub fn is_run_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 19 && b[8] == b'-' && b[15] == b'-' && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit) && b[16..].iter().all(|c| super::ID_CHARS.contains(c))
}

pub fn firm_dir(d: &Daemon, firm: &str) -> PathBuf {
    d.paths.state.join("firms").join(firm)
}

fn utc_day(ts: f64) -> i64 {
    (ts / 86400.0).floor() as i64
}

impl FirmRun {
    pub fn ended(&self) -> bool {
        matches!(*self.state.lock().unwrap(), RunState::Ended | RunState::Stopping)
    }

    pub fn worker(&self, name: &str) -> Result<&Arc<WorkerRt>> {
        self.workers.get(name).ok_or_else(|| anyhow!("no worker {name}"))
    }

    pub fn set_state(&self, worker: &WorkerRt, state: WorkerState, detail: Option<String>) {
        let mut cur = worker.state.lock().unwrap();
        if cur.0 == state && cur.1 == detail {
            return;
        }
        *cur = (state, detail.clone());
        drop(cur);
        self.journal.append("worker", json!({ "worker": worker.name, "state": state, "detail": detail }));
    }

    /// Sends mail and hands it to the recipients' foremen.
    pub fn send(&self, d: Draft) -> Result<mail::Sent> {
        let sent = self.post.lock().unwrap().send(&self.journal, d, now())?;
        self.deliver(&sent);
        Ok(sent)
    }

    pub fn deliver(&self, sent: &mail::Sent) {
        for r in &sent.recipients {
            if let Some(w) = self.workers.get(r) {
                w.queue.lock().unwrap().push(sent.id.clone());
                w.wake.notify_one();
            }
        }
    }

    /// A notice from the post office.
    pub fn notice(&self, to: &str, subject: &str, body: &str) {
        let d = Draft::new(mail::OFFICE, vec![to.to_string()], subject, body, Kind::Fyi);
        // The post office's lock ends with this statement, before the
        // workers' queues are touched.
        let sent = self.post.lock().unwrap().send_notice(&self.journal, d, now());
        match sent {
            Ok(sent) => self.deliver(&sent),
            Err(e) => tracing::warn!(firm = %self.firm, "notice to {to}: {e:#}"),
        }
    }

    /// Whether `worker` may start a turn under the budget; tells the board
    /// once a day when it may not.
    pub fn budget_allows(&self, worker: &str) -> bool {
        let b = &self.config.budget;
        let mut u = self.usage.lock().unwrap();
        let today = utc_day(now());
        if u.day != today {
            *u = Usage { day: today, ..Default::default() };
        }
        let worker_turns = u.worker_turns.get(worker).copied().unwrap_or(0);
        let firm_over = (b.daily_turns > 0 && u.turns >= b.daily_turns) || (b.daily_tokens > 0 && u.tokens >= b.daily_tokens);
        let worker_over = b.worker_daily_turns > 0 && worker_turns >= b.worker_daily_turns;
        if !(firm_over || worker_over) {
            return true;
        }
        if firm_over && !u.notified {
            u.notified = true;
            let body = format!(
                "[Automatic notice] The firm reached its budget: {} of {} turns and {} tokens today. Workers sleep until the reset at midnight UTC, with their mail kept. Raise the budget in the config (firms.{}.budget) or wait.\n",
                u.turns,
                b.daily_turns,
                u.tokens,
                self.firm
            );
            drop(u);
            self.journal.append("budget", json!({ "over": true }));
            self.notice(BOARD, "Budget reached", &body);
        }
        false
    }

    pub fn count_turn(&self, worker: &str, tokens: u64) {
        let mut u = self.usage.lock().unwrap();
        let today = utc_day(now());
        if u.day != today {
            *u = Usage { day: today, ..Default::default() };
        }
        u.turns += 1;
        u.tokens += tokens;
        *u.worker_turns.entry(worker.to_string()).or_default() += 1;
    }

    pub fn status(&self) -> Value {
        let workers: Vec<Value> = self
            .workers
            .values()
            .map(|w| {
                let (state, detail) = w.state.lock().unwrap().clone();
                let po = self.post.lock().unwrap();
                let desk = po.desk(&w.name);
                json!({
                    "name": w.name, "role": w.role, "title": w.title, "officer": w.officer,
                    "state": state, "detail": detail,
                    "session": w.session().map(|s| s.id.clone()),
                    "unread": po.unread(&w.name).len(),
                    "owed": desk.owed.len(), "waiting": desk.waiting.len(),
                    "queued": w.queue.lock().unwrap().len(),
                })
            })
            .collect();
        let u = self.usage.lock().unwrap();
        json!({
            "firm": self.firm, "run": self.id, "state": *self.state.lock().unwrap(), "started": self.started,
            "domain": self.domain, "workers": workers,
            "usage": { "day": u.day, "turns": u.turns, "tokens": u.tokens, "worker_turns": u.worker_turns },
            "budget": self.config.budget,
            "board_unread": self.post.lock().unwrap().unread(BOARD).len(),
            "facilities": self.facility.status(),
        })
    }
}

/// The chart as the UI draws it, written at the start of a run.
fn chart(name: &str, firm: &Firm, config: &Config) -> Value {
    let workers: Vec<Value> = firm
        .workers
        .iter()
        .map(|w| {
            let role = firm.roles.get(&w.role);
            json!({
                "name": w.name, "role": w.role, "title": firm.title_of(&w.name),
                "reports_to": w.reports_to, "officer": firm.is_officer(&w.name),
                "avatar": role.map(|r| json!(r.avatar)), "model": role.and_then(|r| effective_model(r, config)),
            })
        })
        .collect();
    json!({ "firm": name, "description": firm.description, "board": firm.board.name, "domain": firm.domain(name), "workers": workers })
}

/// The model a role's workers run: the role's, else its profile's.
pub fn effective_model(role: &zapper_policy::Role, config: &Config) -> Option<String> {
    role.model.clone().or_else(|| config.resolve(&role.profile).ok().and_then(|p| p.agent.model))
}

/// Memory still available on the host, in MiB.
fn mem_available_mib() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    info.lines().find_map(|l| l.strip_prefix("MemAvailable:")?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()).map(|k| k / 1024)
}

/// Checks that the firm fits before booting anything.
fn check_resources(d: &Daemon, config: &Config, firm: &Firm) -> Result<()> {
    let mut mib = 0u64;
    let mut count = 0usize;
    for w in &firm.workers {
        let role = &firm.roles[&w.role];
        mib += config.resolve(&role.profile)?.engine.memory_mib as u64;
        count += 1;
    }
    for f in firm.facilities.values() {
        mib += config.resolve(&f.profile)?.engine.memory_mib as u64;
        count += 1;
    }
    let running = d.sessions.read().unwrap().values().filter(|s| !matches!(*s.status.borrow(), session::Status::Stopped(_) | session::Status::Failed(_))).count();
    if running + count > config.daemon.max_sessions {
        bail!("the firm needs {count} sessions and {running} are running; daemon.max_sessions is {}", config.daemon.max_sessions);
    }
    if let Some(avail) = mem_available_mib() {
        if mib > avail {
            bail!("the firm needs {mib} MiB of memory for its VMs and the host has {avail} MiB available");
        }
    }
    Ok(())
}

/// Starts a fresh run of a firm: boots its facilities, then its workers.
pub async fn up(d: &Arc<Daemon>, name: &str) -> Result<Arc<FirmRun>> {
    if let Ok(r) = d.office.run(name) {
        if !r.ended() {
            bail!("firm {name} is already running (run {})", r.id);
        }
    }
    let config = d.config.read().unwrap().clone();
    let firm = config.firms.get(name).cloned().ok_or_else(|| anyhow!("no firm {name}"))?;
    check_resources(d, &config, &firm)?;
    let id = run_id();
    let dir = firm_dir(d, name).join("runs").join(&id);
    for sub in ["transcripts", "drive", "skills", "empty"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    let journal = Journal::create(&dir.join("journal.jsonl"))?;
    let domain = firm.domain(name);
    let roles = firm.roles.keys().map(|r| (r.clone(), Arc::new(RwLock::new(Policy::default())))).collect();
    let workers = firm
        .workers
        .iter()
        .map(|w| {
            let rt = WorkerRt {
                name: w.name.clone(),
                role: w.role.clone(),
                title: firm.title_of(&w.name),
                officer: firm.is_officer(&w.name),
                session: Mutex::new(None),
                state: Mutex::new((WorkerState::Off, None)),
                queue: Mutex::new(vec![]),
                notes: Mutex::new(vec![]),
                access_queue: Mutex::new(vec![]),
                digest_due: AtomicBool::new(false),
                wake: Notify::new(),
                context: Mutex::new(None),
            };
            (w.name.clone(), Arc::new(rt))
        })
        .collect();
    let started = now();
    let run = Arc::new(FirmRun {
        firm: name.to_string(),
        id: id.clone(),
        dir: dir.clone(),
        post: Mutex::new(PostOffice::new(&firm)),
        drive: Mutex::new(drive::Drive::new(&firm)?),
        security: Mutex::new(security::Security::default()),
        roles,
        turns: Arc::new(Semaphore::new(firm.budget.max_concurrent as usize)),
        usage: Mutex::new(Usage { day: utc_day(started), ..Default::default() }),
        facility: facility::Facilities::default(),
        config: firm.clone(),
        domain: domain.clone(),
        started,
        state: Mutex::new(RunState::Starting),
        journal,
        workers,
        stopping: Notify::new(),
    });
    let info = json!({ "firm": name, "run": id, "started": started, "chart": chart(name, &firm, &config), "budget": firm.budget, "agent_version": env!("CARGO_PKG_VERSION") });
    super::write_private(&dir.join("run.json"), serde_json::to_string_pretty(&info)?.as_bytes())?;
    // The whole config the run started with, for replay: profiles, rules,
    // firms. Inline secret values are hidden, as in `zapper config startup`.
    super::write_private(&dir.join(RUN_CONFIG), super::redact(config.clone()).to_yaml().as_bytes())?;
    run.journal.append("run", json!({ "state": "starting", "chart": chart(name, &firm, &config), "budget": firm.budget }));
    d.office.runs.write().unwrap().insert(name.to_string(), run.clone());

    if let Err(e) = boot(d, &run, &config).await {
        let msg = format!("{e:#}");
        run.journal.append("run", json!({ "state": "failed", "error": msg }));
        down(d, &run, "boot failed").await;
        bail!("firm {name}: {msg}");
    }
    *run.state.lock().unwrap() = RunState::Running;
    run.journal.append("run", json!({ "state": "running" }));
    for w in run.workers.values() {
        tokio::spawn(foreman::run(d.clone(), run.clone(), w.clone()));
    }
    tokio::spawn(ticker(run.clone()));
    Ok(run)
}

async fn boot(d: &Arc<Daemon>, run: &Arc<FirmRun>, config: &Config) -> Result<()> {
    let firm = &run.config;
    facility::start(d, run, config).await?;
    skills::prepare(run, config)?;
    let mut tasks = Vec::new();
    for w in run.workers.values().cloned() {
        let (d, run) = (d.clone(), run.clone());
        let role = firm.roles[&w.role].clone();
        let config = config.clone();
        tasks.push(tokio::spawn(async move {
            run.set_state(&w, WorkerState::Booting, None);
            let r = boot_worker(&d, &run, &config, &w, &role).await;
            if let Err(e) = &r {
                run.set_state(&w, WorkerState::Error, Some(format!("{e:#}")));
            }
            r.with_context(|| format!("worker {}", w.name))
        }));
    }
    let mut first_err = None;
    for t in tasks {
        if let Err(e) = t.await.map_err(|e| anyhow!("{e}")).and_then(|r| r) {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

async fn boot_worker(d: &Arc<Daemon>, run: &Arc<FirmRun>, config: &Config, w: &Arc<WorkerRt>, role: &zapper_policy::Role) -> Result<()> {
    let profile = config.resolve(&role.profile)?;
    let user = profile.agent.user.clone();
    let home = format!("/home/{user}");
    let services = facility::services_for(run, &w.role);
    let mut files = vec![GuestFile { path: format!("{home}/.claude/CLAUDE.md"), content: prompts::preamble(&run.firm, &run.config, &w.name, &services), mode: 0o644, user_owned: true }];
    files.extend(facility::credentials_for(run, &w.name, &w.role, &home)?);
    // A headless browser command, for the image's Playwright.
    files.push(GuestFile { path: "/usr/local/bin/browse".into(), content: include_str!("browse.js").into(), mode: 0o755, user_owned: false });
    if let Some(script) = &role.fake {
        let text = std::fs::read_to_string(zapper_policy::expand_home(script)).with_context(|| format!("fake script {script}"))?;
        files.push(GuestFile { path: format!("{home}/.office/fake.yaml"), content: text, mode: 0o644, user_owned: true });
    }
    let mut overlay = json!({
        "mcp_servers": { "office": { "command": ops::AGENT_PATH, "args": ["office", "mcp"] } },
    });
    if let Some(m) = &role.model {
        overlay["model"] = json!(m);
    }
    let skills_dir = run.dir.join("skills").join(&w.role);
    let link = FirmLink { firm: run.firm.clone(), member: w.name.clone(), worker: true, run: Arc::downgrade(run), role: Some((w.role.clone(), run.roles[&w.role].clone())) };
    let new = NewSession {
        profile: role.profile.clone(),
        workdir: run.dir.join("empty"),
        policy: Policy::default(),
        firm: Some(link),
        files,
        mounts: vec![Mount { host: skills_dir.to_string_lossy().into_owned(), guest: format!("{home}/.claude/skills"), mode: MountMode::Ro, extra: None }],
        agent_overlay: Some(overlay),
        forwards: vec![],
    };
    let s = session::create(d.clone(), new).await?;
    run.journal.append("worker_session", json!({ "worker": w.name, "session": s.id }));
    *w.session.lock().unwrap() = Some(s);
    Ok(())
}

/// Follow-ups, digests and expiries.
async fn ticker(run: Arc<FirmRun>) {
    let mut last_digest = now();
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = run.stopping.notified() => return,
        }
        if run.ended() {
            return;
        }
        let sent = run.post.lock().unwrap().tick(&run.journal, now());
        for s in &sent {
            run.deliver(s);
        }
        security::tick(&run, &mut last_digest);
    }
}

/// Stops a run: its workers' sessions, its facilities, its tasks.
pub async fn down(d: &Arc<Daemon>, run: &Arc<FirmRun>, reason: &str) {
    {
        let mut st = run.state.lock().unwrap();
        if matches!(*st, RunState::Ended) {
            return;
        }
        *st = RunState::Stopping;
    }
    run.stopping.notify_waiters();
    for w in run.workers.values() {
        w.wake.notify_one();
    }
    let mut tasks = Vec::new();
    for w in run.workers.values() {
        if let Some(s) = w.session.lock().unwrap().take() {
            let d = d.clone();
            tasks.push(tokio::spawn(async move {
                session::stop(&d, &s, "firm down").await;
                d.sessions.write().unwrap().remove(&s.id);
            }));
        }
        run.set_state(w, WorkerState::Off, None);
    }
    for t in tasks {
        let _ = t.await;
    }
    facility::stop(d, run).await;
    *run.state.lock().unwrap() = RunState::Ended;
    run.journal.append("run", json!({ "state": "ended", "reason": reason }));
}

pub fn pause(run: &FirmRun, paused: bool) -> Result<()> {
    let mut st = run.state.lock().unwrap();
    match (&*st, paused) {
        (RunState::Running, true) => *st = RunState::Paused,
        (RunState::Paused, false) => *st = RunState::Running,
        (RunState::Running, false) | (RunState::Paused, true) => return Ok(()),
        (s, _) => bail!("the run is {s:?}"),
    }
    drop(st);
    run.journal.append("run", json!({ "state": if paused { "paused" } else { "running" } }));
    for w in run.workers.values() {
        w.wake.notify_one();
    }
    Ok(())
}

/// The board sends mail.
pub fn board_send(run: &FirmRun, v: &Value) -> Result<mail::Sent> {
    let d = ops::draft_from(BOARD, v, None, &run.post.lock().unwrap())?;
    run.send(d)
}

/// Ends the journals of runs a previous daemon left running: they can't be
/// resumed, and a replay should say how they ended.
pub fn close_orphans(state: &std::path::Path) {
    let Ok(firms) = std::fs::read_dir(state.join("firms")) else { return };
    for firm in firms.flatten() {
        let Ok(runs) = std::fs::read_dir(firm.path().join("runs")) else { continue };
        for run in runs.flatten() {
            let path = run.path().join("journal.jsonl");
            let Ok(text) = std::fs::read(&path) else { continue };
            let tail = &text[text.len().saturating_sub(64 * 1024)..];
            let ended = String::from_utf8_lossy(tail)
                .lines()
                .rev()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find(|v| v["type"] == "run")
                .is_some_and(|v| v["state"] == "ended" || v["state"] == "failed");
            if !ended {
                if let Ok(j) = Journal::create(&path) {
                    j.append("run", json!({ "state": "ended", "reason": "the daemon stopped" }));
                }
            }
        }
    }
}

/// Past runs of a firm, newest first.
pub fn list_runs(d: &Daemon, firm: &str) -> Vec<Value> {
    let dir = firm_dir(d, firm).join("runs");
    let mut out: Vec<Value> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_run_id(&name) {
                return None;
            }
            let info: Value = std::fs::read(e.path().join("run.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
            let journal = e.path().join("journal.jsonl");
            let size = std::fs::metadata(&journal).map(|m| m.len()).unwrap_or(0);
            let sum = run_summary(&journal);
            Some(json!({
                "run": name, "started": info["started"], "journal_bytes": size,
                "kickoff": sum.kickoff, "messages": sum.messages, "last": sum.last, "ended": sum.ended,
            }))
        })
        .collect();
    out.sort_by(|a, b| b["run"].as_str().cmp(&a["run"].as_str()));
    if let Ok(r) = d.office.run(firm) {
        for v in out.iter_mut() {
            if v["run"] == json!(r.id) {
                v["state"] = json!(*r.state.lock().unwrap());
            }
        }
    }
    out
}

/// The config a run started with, in its directory.
pub const RUN_CONFIG: &str = "config.yaml";

/// A run's config and the skills its workers had: what it was run with.
pub fn run_config(dir: &std::path::Path) -> Value {
    let config = std::fs::read_to_string(dir.join(RUN_CONFIG)).ok();
    // skills/<role>/<skill>/…: SKILL.md as text, other files by name.
    let mut skills = Vec::new();
    let mut roles: Vec<_> = std::fs::read_dir(dir.join("skills")).into_iter().flatten().flatten().filter(|e| e.path().is_dir()).collect();
    roles.sort_by_key(|e| e.file_name());
    for role in roles {
        let mut list: Vec<_> = std::fs::read_dir(role.path()).into_iter().flatten().flatten().filter(|e| e.path().is_dir()).collect();
        list.sort_by_key(|e| e.file_name());
        for sk in list {
            let text = std::fs::read(sk.path().join("SKILL.md")).ok().map(|b| String::from_utf8_lossy(&b[..b.len().min(256 << 10)]).into_owned());
            let mut files: Vec<String> = std::fs::read_dir(sk.path()).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != "SKILL.md").collect();
            files.sort();
            skills.push(json!({ "role": role.file_name().to_string_lossy(), "skill": sk.file_name().to_string_lossy(), "text": text, "files": files }));
        }
    }
    json!({ "config": config, "skills": skills })
}

/// What a run's journal says about it, for the list of runs.
struct RunSummary {
    /// The board's first message: what the firm was asked to build. A run
    /// without one was never kicked off.
    kickoff: Option<Value>,
    messages: u64,
    last: Option<f64>,
    ended: bool,
}

fn run_summary(journal: &std::path::Path) -> RunSummary {
    use std::io::BufRead;
    let mut sum = RunSummary { kickoff: None, messages: 0, last: None, ended: false };
    let Ok(f) = std::fs::File::open(journal) else { return sum };
    for line in std::io::BufReader::new(f).split(b'\n').map_while(Result::ok) {
        // Most lines are not needed whole: look before parsing.
        let mail = has(&line, br#""type":"mail""#);
        let run = has(&line, br#""type":"run""#);
        if !mail && !run && sum.last.is_some() && line.len() > 64 {
            if let Some(ts) = ts_of(&line) {
                sum.last = Some(ts);
            }
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        if let Some(ts) = v["ts"].as_f64() {
            sum.last = Some(ts);
        }
        if mail {
            sum.messages += 1;
            if sum.kickoff.is_none() && v["from"] == BOARD {
                let body = v["body"].as_str().unwrap_or("");
                let preview: String = body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect();
                sum.kickoff = Some(json!({ "subject": v["subject"], "preview": preview, "to": v["to"], "ts": v["ts"] }));
            }
        } else if run {
            sum.ended = matches!(v["state"].as_str(), Some("ended" | "failed"));
        }
    }
    sum
}

fn has(line: &[u8], pat: &[u8]) -> bool {
    line.windows(pat.len()).any(|w| w == pat)
}

/// `"ts":<number>` from a journal line, without parsing it all.
fn ts_of(line: &[u8]) -> Option<f64> {
    let key = br#""ts":"#;
    let at = line.windows(key.len()).position(|w| w == key)? + key.len();
    let rest = &line[at..];
    let end = rest.iter().position(|b| !(b.is_ascii_digit() || *b == b'.' || *b == b'-' || *b == b'e' || *b == b'E' || *b == b'+')).unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end]).ok()?.parse().ok()
}

/// A run's directory; `current` is the firm's current or last run.
pub fn run_dir(d: &Daemon, firm: &str, run: &str) -> Result<PathBuf> {
    if !zapper_policy::valid_worker_name(firm) {
        bail!("{firm:?} is not a firm name");
    }
    let id = if run == "current" { d.office.run(firm)?.id.clone() } else { run.to_string() };
    if !is_run_id(&id) {
        bail!("{run:?} is not a run id");
    }
    let dir = firm_dir(d, firm).join("runs").join(&id);
    if !dir.is_dir() {
        bail!("no run {id} of firm {firm}");
    }
    Ok(dir)
}

pub fn prune(d: &Daemon, firm: &str, run: &str) -> Result<()> {
    let dir = run_dir(d, firm, run)?;
    if let Ok(r) = d.office.run(firm) {
        if r.dir == dir && !r.ended() {
            bail!("run {run} is running; stop the firm first");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_summaries() {
        let dir = std::env::temp_dir().join(format!("zapper-runsum-{}", crate::daemon::random_id(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let j = dir.join("journal.jsonl");
        std::fs::write(&j, concat!(
            r#"{"seq":1,"ts":10.0,"type":"run","state":"running"}"#, "\n",
            r#"{"seq":2,"ts":11.5,"type":"mail","from":"ceo","to":["ada"],"subject":"x","body":"y"}"#, "\n",
            r#"{"seq":3,"ts":12.0,"type":"mail","from":"board","to":["ceo"],"subject":"Build a todo app","body":"An MVP,\n  please."}"#, "\n",
            r#"{"seq":4,"ts":20.25,"type":"worker","worker":"ceo","state":"working","detail":"thinking about the todo list app"}"#, "\n",
            r#"{"seq":5,"ts":30.0,"type":"run","state":"ended"}"#, "\n",
        )).unwrap();
        let s = super::run_summary(&j);
        assert_eq!(s.messages, 2);
        assert_eq!(s.kickoff.as_ref().unwrap()["subject"], "Build a todo app");
        assert_eq!(s.kickoff.as_ref().unwrap()["preview"], "An MVP, please.");
        assert_eq!(s.last, Some(30.0));
        assert!(s.ended);
        std::fs::write(&j, r#"{"seq":1,"ts":10.0,"type":"run","state":"running"}"#).unwrap();
        assert!(super::run_summary(&j).kickoff.is_none(), "a run the board never wrote to has no kickoff");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_ids() {
        let id = super::run_id();
        assert!(super::is_run_id(&id), "{id}");
        assert!(!super::is_run_id("../../etc"));
        assert!(!super::is_run_id("20261016-140501-K3Z"));
    }
}
