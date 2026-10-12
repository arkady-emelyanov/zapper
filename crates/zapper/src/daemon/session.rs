//! One sandbox: a microVM, its guest channel, policy layer and audit trail.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Child;
use tokio::sync::{mpsc, watch, Semaphore};
use zapper_policy::proto::{Frame, GuestDisk, GuestMsg, HostMsg, ShellRequest, CONTROL_PORT, MAX_FRAME, MAX_LINE, PROXY_PORT, SHELL_PORT};
use zapper_policy::{evaluate_exec, Action, AnyRule, Config, ExecRequest, ExecRule, Kind, MountMode, Policy, ResolvedProfile, Scope};

use super::{now, proxy, spec, tls, vm, Daemon, Event, EventBody};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "lowercase")]
pub enum Status {
    Starting,
    Running,
    Failed(String),
    Stopped(String),
}

#[derive(Default, Serialize)]
pub struct Counters {
    pub net_allowed: AtomicU64,
    pub net_denied: AtomicU64,
    pub exec_allowed: AtomicU64,
    pub exec_denied: AtomicU64,
    pub exits: AtomicU64,
    pub shells: AtomicU64,
}

pub struct Session {
    pub id: String,
    pub profile: String,
    pub created: f64,
    pub expires: Option<f64>,
    pub resolved: ResolvedProfile,
    pub workdir: PathBuf,
    /// The session's own policy layer, above its profile. Not saved.
    pub policy: RwLock<Policy>,
    pub status: watch::Sender<Status>,
    pub dir: PathBuf,
    pub ca: tls::SessionCa,
    pub counters: Counters,
    pub usage: Mutex<Option<zapper_policy::proto::GuestEvent>>,
    /// Names given out by the session's DNS, by synthetic address.
    pub dns: Mutex<super::dns::FakeDns>,
    guest: Mutex<Option<mpsc::Sender<HostMsg>>>,
    vm: tokio::sync::Mutex<Option<Child>>,
    audit: Mutex<Option<std::fs::File>>,
    next_shell: AtomicU64,
    /// Bounds concurrent proxy connections from the guest.
    pub conns: Arc<Semaphore>,
    exec_checks: Arc<Semaphore>,
}

#[derive(Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub profile: String,
    #[serde(flatten)]
    pub status: Status,
    pub created: f64,
    pub expires: Option<f64>,
    pub image: String,
    pub vcpus: u8,
    pub memory_mib: u32,
    pub user: String,
    pub root: bool,
    pub workdir: String,
    pub counters: serde_json::Value,
    pub usage: Option<zapper_policy::proto::GuestEvent>,
    pub rules: usize,
}

impl Session {
    pub fn info(&self) -> SessionInfo {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        SessionInfo {
            id: self.id.clone(),
            profile: self.profile.clone(),
            status: self.status.borrow().clone(),
            created: self.created,
            expires: self.expires,
            image: self.resolved.engine.image.clone(),
            vcpus: self.resolved.engine.vcpus,
            memory_mib: self.resolved.engine.memory_mib,
            user: self.resolved.agent.user.clone(),
            root: self.resolved.agent.root,
            workdir: self.workdir.display().to_string(),
            counters: serde_json::json!({
                "net_allowed": get(&c.net_allowed), "net_denied": get(&c.net_denied),
                "exec_allowed": get(&c.exec_allowed), "exec_denied": get(&c.exec_denied),
                "exits": get(&c.exits), "shells": get(&c.shells),
            }),
            usage: self.usage.lock().unwrap().clone(),
            rules: self.policy.read().unwrap().rules().len(),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(*self.status.borrow(), Status::Running)
    }

    pub fn record(&self, event: &Event) {
        if let Some(f) = self.audit.lock().unwrap().as_mut() {
            let mut line = serde_json::to_vec(event).unwrap_or_default();
            line.push(b'\n');
            let _ = f.write_all(&line);
        }
    }

    pub fn audit_path(&self) -> PathBuf {
        self.dir.join("audit.jsonl")
    }

    /// Policy layers, most specific first. Uses the running config, so rule
    /// changes apply to running sessions at once; a profile removed from the
    /// running config falls back to its layers at session start.
    pub fn layers(&self, config: &Config) -> Vec<(Scope, Policy)> {
        let mut v = vec![(Scope::Session(self.id.clone()), self.policy.read().unwrap().clone())];
        match config.resolve(&self.profile) {
            Ok(p) => v.extend(p.layers),
            Err(_) => v.extend(self.resolved.layers.iter().cloned()),
        }
        v.push((Scope::Global, config.policy.clone()));
        v
    }

    /// Adds a rule to the session layer under a fresh `s-<kind>-<n>` id, so
    /// session rule ids never clash with config ones.
    pub fn add_rule(&self, mut rule: AnyRule, position: Option<usize>) -> AnyRule {
        let mut policy = self.policy.write().unwrap();
        let taken: Vec<String> = policy.rules().iter().filter_map(|r| r.id().map(str::to_string)).collect();
        let kind = rule.kind();
        let id = (1..).map(|n| format!("s-{kind}-{n}")).find(|i| !taken.contains(i)).unwrap();
        rule.set_id(id);
        policy.insert(rule.clone(), position);
        rule
    }

    fn send(&self, msg: HostMsg) {
        if let Some(tx) = self.guest.lock().unwrap().as_ref() {
            let _ = tx.try_send(msg);
        }
    }

    /// Opens a terminal in the guest.
    pub async fn open_shell(&self, mut req: ShellRequest) -> Result<(UnixStream, u64)> {
        if !self.is_running() {
            bail!("session {} is not running", self.id);
        }
        if req.argv.is_empty() {
            req.argv = self.resolved.agent.command.clone();
        }
        let id = self.next_shell.fetch_add(1, Ordering::Relaxed);
        req.id = id;
        let mut stream = UnixStream::connect(vm::vsock_path(&self.dir)).await?;
        stream.write_all(format!("CONNECT {SHELL_PORT}\n").as_bytes()).await?;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while line.len() < 64 {
            stream.read_exact(&mut byte).await?;
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        if !line.starts_with(b"OK ") {
            bail!("guest refused terminal: {}", String::from_utf8_lossy(&line));
        }
        let mut json = serde_json::to_vec(&req)?;
        json.push(b'\n');
        stream.write_all(&json).await?;
        self.counters.shells.fetch_add(1, Ordering::Relaxed);
        Ok((stream, id))
    }
}

pub struct NewSession {
    pub profile: String,
    pub workdir: PathBuf,
    pub policy: Policy,
}

/// Creates a session and boots its VM; returns once the guest is ready.
pub async fn create(daemon: Arc<Daemon>, new: NewSession) -> Result<Arc<Session>> {
    let (resolved, max) = {
        let c = daemon.config.read().unwrap();
        (c.resolve(&new.profile)?, c.daemon.max_sessions)
    };
    let running = daemon.sessions.read().unwrap().values().filter(|s| matches!(*s.status.borrow(), Status::Starting | Status::Running)).count();
    if running >= max {
        bail!("{running} sessions running; daemon.max_sessions is {max}");
    }
    let id = super::random_id(6);
    let dir = daemon.paths.state.join("sessions").join(&id);
    std::fs::create_dir_all(&dir)?;
    let audit = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("audit.jsonl"))?;
    let created = now();
    let ttl = resolved.engine.ttl_secs;
    let session = Arc::new(Session {
        id: id.clone(),
        profile: new.profile.clone(),
        created,
        expires: (ttl > 0).then(|| created + ttl as f64),
        workdir: new.workdir.clone(),
        policy: RwLock::new(Policy::default()),
        status: watch::channel(Status::Starting).0,
        ca: tls::SessionCa::new(&id)?,
        dir: dir.clone(),
        resolved,
        counters: Counters::default(),
        usage: Mutex::new(None),
        dns: Mutex::new(Default::default()),
        guest: Mutex::new(None),
        vm: tokio::sync::Mutex::new(None),
        audit: Mutex::new(Some(audit)),
        next_shell: AtomicU64::new(1),
        conns: Arc::new(Semaphore::new(256)),
        exec_checks: Arc::new(Semaphore::new(64)),
    });
    for rule in new.policy.rules() {
        session.add_rule(rule, None);
    }
    daemon.sessions.write().unwrap().insert(id.clone(), session.clone());
    daemon.emit(Some(&session), EventBody::Session { state: "starting".into(), detail: Some(format!("profile {}", new.profile)) });

    let started = Instant::now();
    if let Err(e) = boot(&daemon, &session).await {
        let msg = format!("{e:#}");
        session.status.send_replace(Status::Failed(msg.clone()));
        daemon.emit(Some(&session), EventBody::Session { state: "failed".into(), detail: Some(msg.clone()) });
        teardown(&daemon, &session, None).await;
        return Err(anyhow!(msg));
    }
    let mut status = session.status.subscribe();
    let ready = tokio::time::timeout(Duration::from_secs(120), status.wait_for(|s| !matches!(s, Status::Starting))).await.is_ok();
    let state = session.status.borrow().clone();
    match (ready, state) {
        (true, Status::Running) => {
            let ms = started.elapsed().as_millis();
            daemon.emit(Some(&session), EventBody::Session { state: "running".into(), detail: Some(format!("ready in {ms} ms")) });
        }
        (_, state) => {
            let msg = match state {
                Status::Failed(m) => m,
                _ => "guest did not become ready in 120s".into(),
            };
            stop(&daemon, &session, &msg).await;
            return Err(anyhow!("session {id}: {msg}; see {}", dir.join("console.log").display()));
        }
    }
    if ttl > 0 {
        let (d, s) = (daemon.clone(), session.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(ttl)).await;
            stop(&d, &s, "ttl expired").await;
        });
    }
    Ok(session)
}

async fn boot(daemon: &Arc<Daemon>, session: &Arc<Session>) -> Result<()> {
    let dir = &session.dir;
    let engine = &session.resolved.engine;
    let root = dir.join("root.ext4");
    vm::root_disk(&daemon.paths.images.join(format!("{}.ext4", engine.image)), &root, vm::parse_size(&engine.disk)?)
        .await
        .context("root disk")?;
    let mut disks = Vec::new();
    let mut guest_disks = Vec::new();
    for (i, m) in session.resolved.mounts.iter().enumerate() {
        let path = dir.join(format!("disk{i}.ext4"));
        let host = vm::mount_disk(m, &session.workdir, &path).await.with_context(|| format!("mount {}", m.host))?;
        let ro = m.mode == MountMode::Ro;
        disks.push((path, ro));
        let device = format!("/dev/vd{}", (b'b' + i as u8) as char);
        guest_disks.push(GuestDisk { device, path: m.guest.clone(), read_only: ro });
        tracing::info!(session = %session.id, "mount {} -> {}", host.display(), m.guest);
    }
    let spec = {
        let c = daemon.config.read().unwrap();
        spec::build(&c, &session.resolved, &session.id, &session.ca.pem, guest_disks)?
    };

    // Guest-initiated vsock connections arrive on `<uds>_<port>`.
    let vsock = vm::vsock_path(dir);
    let control_path = PathBuf::from(format!("{}_{CONTROL_PORT}", vsock.display()));
    let proxy_path = PathBuf::from(format!("{}_{PROXY_PORT}", vsock.display()));
    let control = UnixListener::bind(&control_path)?;
    let proxy = UnixListener::bind(&proxy_path)?;
    let nic = engine.network == zapper_policy::NetworkMode::Nic;
    if nic {
        let listener = UnixListener::bind(dir.join(crate::netns::SOCKET))?;
        tokio::spawn(super::nic::serve(daemon.clone(), session.clone(), listener));
    }

    let child = vm::start(vm::VmSpec {
        firecracker: &daemon.paths.firecracker,
        kernel: daemon.paths.kernels.join(&engine.kernel),
        initrd: &daemon.paths.initrd,
        root,
        disks,
        engine,
        dir,
        nic,
    })
    .await?;
    *session.vm.lock().await = Some(child);

    {
        let (d, s) = (daemon.clone(), session.clone());
        tokio::spawn(async move {
            if let Err(e) = guest_control(&d, &s, control, control_path, spec).await {
                tracing::warn!(session = %s.id, "guest control: {e:#}");
                if matches!(*s.status.borrow(), Status::Starting) {
                    s.status.send_replace(Status::Failed(format!("{e:#}")));
                }
            }
        });
    }
    {
        let (d, s) = (daemon.clone(), session.clone());
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = proxy.accept().await else { break };
                let Ok(permit) = s.conns.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let (d, s) = (d.clone(), s.clone());
                tokio::spawn(async move {
                    proxy::serve(d, s, stream).await;
                    drop(permit);
                });
            }
        });
    }
    {
        // VM exit watcher.
        let (d, s) = (daemon.clone(), session.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let mut vm = s.vm.lock().await;
                let Some(child) = vm.as_mut() else { return };
                if let Ok(Some(status)) = child.try_wait() {
                    *vm = None;
                    drop(vm);
                    let reason = format!("vm exited ({status})");
                    if matches!(*s.status.borrow(), Status::Starting) {
                        s.status.send_replace(Status::Failed(reason.clone()));
                    }
                    stop(&d, &s, &reason).await;
                    return;
                }
            }
        });
    }
    Ok(())
}

async fn guest_control(daemon: &Arc<Daemon>, session: &Arc<Session>, listener: UnixListener, path: PathBuf, spec: zapper_policy::proto::GuestSpec) -> Result<()> {
    let (stream, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept()).await.context("guest did not connect")??;
    // Only the first connection is the agent: anything later could be the
    // workload. Remove the socket so further guest connections are refused.
    drop(listener);
    let _ = std::fs::remove_file(&path);

    let (r, mut w) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<HostMsg>(256);
    tx.send(HostMsg::Init { spec: Box::new(spec) }).await?;
    *session.guest.lock().unwrap() = Some(tx.clone());
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut line = serde_json::to_vec(&msg).unwrap();
            line.push(b'\n');
            if w.write_all(&line).await.is_err() {
                break;
            }
        }
    });

    let mut reader = BufReader::new(r);
    loop {
        let mut line = Vec::new();
        let n = (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line).await?;
        if n == 0 {
            break;
        }
        if n > MAX_LINE {
            bail!("guest sent an oversized message");
        }
        let msg: GuestMsg = match serde_json::from_slice(&line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(session = %session.id, "bad guest message: {e}");
                continue;
            }
        };
        match msg {
            GuestMsg::Ready => {
                session.status.send_replace(Status::Running);
            }
            GuestMsg::Failed { error } => {
                session.status.send_replace(Status::Failed(format!("guest setup: {error}")));
            }
            GuestMsg::ExecCheck { id, pid, uid, req } => {
                let Ok(permit) = session.exec_checks.clone().try_acquire_owned() else {
                    let _ = tx.try_send(HostMsg::ExecReply { id, allow: false });
                    continue;
                };
                let (d, s, tx) = (daemon.clone(), session.clone(), tx.clone());
                tokio::spawn(async move {
                    let allow = decide_exec(&d, &s, pid, uid, req).await;
                    let _ = tx.send(HostMsg::ExecReply { id, allow }).await;
                    drop(permit);
                });
            }
            GuestMsg::Event { event } => {
                use zapper_policy::proto::GuestEvent;
                match &event {
                    GuestEvent::Usage { .. } => {
                        *session.usage.lock().unwrap() = Some(event);
                        continue;
                    }
                    GuestEvent::Exit { .. } => {
                        session.counters.exits.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {}
                }
                daemon.emit(Some(session), EventBody::Guest { event });
            }
        }
    }
    Ok(())
}

async fn decide_exec(daemon: &Arc<Daemon>, session: &Arc<Session>, pid: i32, uid: u32, req: ExecRequest) -> bool {
    let started = Instant::now();
    let decision = {
        let c = daemon.config.read().unwrap();
        let layers = session.layers(&c);
        evaluate_exec(&spec::layers_of(&layers), &req)
    };
    let allow = match decision.action {
        Action::Allow => true,
        Action::Deny => false,
        Action::Ask => {
            let suggestion = AnyRule::Exec(ExecRule {
                id: None,
                action: Action::Allow,
                path: req.path.clone(),
                args: None,
                final_: false,
                comment: Some("approved".into()),
            });
            daemon.ask(session, Kind::Exec, format!("exec {}", req.argv.join(" ")), suggestion).await
        }
    };
    let counter = if allow { &session.counters.exec_allowed } else { &session.counters.exec_denied };
    counter.fetch_add(1, Ordering::Relaxed);
    daemon.emit(
        Some(session),
        EventBody::Exec {
            pid,
            uid,
            path: req.path,
            argv: req.argv,
            cwd: req.cwd,
            action: if allow { Action::Allow } else { Action::Deny },
            rule: decision.rule,
            scope: decision.scope.map(|s| s.to_string()),
            wait_ms: started.elapsed().as_secs_f64() * 1000.0,
        },
    );
    allow
}

/// Stops a session's VM and removes its disks; the audit log, console log and
/// recordings stay.
pub async fn stop(daemon: &Arc<Daemon>, session: &Arc<Session>, reason: &str) {
    {
        let current = session.status.borrow().clone();
        if matches!(current, Status::Stopped(_)) {
            return;
        }
        if !matches!(current, Status::Failed(_)) {
            session.status.send_replace(Status::Stopped(reason.into()));
        }
    }
    session.send(HostMsg::Shutdown);
    teardown(daemon, session, Some(reason)).await;
}

async fn teardown(daemon: &Arc<Daemon>, session: &Arc<Session>, reason: Option<&str>) {
    if let Some(mut child) = session.vm.lock().await.take() {
        if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
    }
    *session.guest.lock().unwrap() = None;
    if let Some(reason) = reason {
        daemon.emit(Some(session), EventBody::Session { state: "stopped".into(), detail: Some(reason.into()) });
    }
    if let Ok(entries) = std::fs::read_dir(&session.dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".ext4") || name.starts_with("v.sock") || name == crate::netns::SOCKET {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    *session.audit.lock().unwrap() = None;
}

/// Pumps frames between a client and a guest terminal, recording the
/// terminal as an asciicast v2 file in the session directory.
pub async fn pump_shell<C>(daemon: Arc<Daemon>, session: Arc<Session>, client: C, guest: UnixStream, id: u64, argv: Vec<String>, tty: Option<(u16, u16)>)
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    daemon.emit(Some(&session), EventBody::Shell { id, state: "open".into(), argv: Some(argv.clone()), exit_code: None });
    let cast = Arc::new(Mutex::new(Recorder::new(&session.dir, id, tty, &argv)));
    let (mut cr, mut cw) = tokio::io::split(client);
    let (mut gr, mut gw) = guest.into_split();
    let rec = cast.clone();
    let upstream = tokio::spawn(async move {
        while let Ok(Some(frame)) = read_frame(&mut cr).await {
            match &frame {
                Frame::Data(d) => rec.lock().unwrap().write("i", d),
                Frame::Resize(r, c) => rec.lock().unwrap().write("r", format!("{c}x{r}").as_bytes()),
                _ => {}
            }
            let mut buf = Vec::new();
            frame.write_to(&mut buf).ok();
            if gw.write_all(&buf).await.is_err() {
                break;
            }
        }
        let _ = gw.shutdown().await;
    });
    let mut exit_code = None;
    while let Ok(Some(frame)) = read_frame(&mut gr).await {
        match &frame {
            Frame::Data(d) | Frame::Stderr(d) => cast.lock().unwrap().write("o", d),
            Frame::Exit(c) => exit_code = Some(*c),
            _ => {}
        }
        let mut buf = Vec::new();
        frame.write_to(&mut buf).ok();
        if cw.write_all(&buf).await.is_err() || exit_code.is_some() {
            break;
        }
    }
    let _ = cw.shutdown().await;
    upstream.abort();
    daemon.emit(Some(&session), EventBody::Shell { id, state: "closed".into(), argv: None, exit_code });
}

pub async fn read_frame(r: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<Option<Frame>> {
    let mut header = [0u8; 5];
    match r.read_exact(&mut header).await {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    };
    let len = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).await?;
    Frame::decode(header[0], body).map(Some)
}

/// asciicast v2 writer.
struct Recorder {
    file: Option<std::fs::File>,
    start: Instant,
}

impl Recorder {
    fn new(dir: &Path, id: u64, tty: Option<(u16, u16)>, argv: &[String]) -> Self {
        let file = std::fs::File::create(dir.join(format!("shell-{id}.cast"))).ok();
        let (rows, cols) = tty.unwrap_or((24, 80));
        let mut r = Recorder { file, start: Instant::now() };
        let header = serde_json::json!({ "version": 2, "width": cols, "height": rows, "timestamp": now() as u64, "command": argv.join(" ") });
        if let Some(f) = r.file.as_mut() {
            let _ = writeln!(f, "{header}");
        }
        r
    }

    fn write(&mut self, kind: &str, data: &[u8]) {
        if let Some(f) = self.file.as_mut() {
            let t = self.start.elapsed().as_secs_f64();
            let line = serde_json::json!([t, kind, String::from_utf8_lossy(data)]);
            let _ = writeln!(f, "{line}");
        }
    }
}
