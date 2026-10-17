//! Firms: an org chart of workers, each a Claude Code in its own session, with
//! the roles, budgets and services the office layer runs them with.
//!
//! The config names the founders (the workers who boot with the firm); a
//! firm with a seed budget grows by hiring from its roles while it runs. See
//! `docs/office.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use crate::{Action, Config};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Firm {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub board: Board,
    /// Names the firm's services: `git.<domain>`, `cluster.<domain>`,
    /// `apps.<domain>`. Empty means `<firm>.internal`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub domain: String,
    /// Local address forwarded to the cluster's ingress, for the board's
    /// browser, e.g. `127.0.0.1:8080`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_port: Option<String>,
    pub budget: Budget,
    pub limits: Limits,
    pub roles: BTreeMap<String, Role>,
    pub workers: Vec<Worker>,
    pub security: Security,
    /// An empty git repository per run at `https://git.<domain>/product.git`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitService>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub facilities: BTreeMap<String, Facility>,
    /// Local address of the cluster's dashboard (Headlamp), for the board,
    /// e.g. `127.0.0.1:8081`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard_port: Option<String>,
    /// The page the cluster serves before the firm deploys anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing: Option<Landing>,
    /// Roles whose VMs get credentials for the firm's services at boot.
    #[serde(skip_serializing_if = "Credentials::is_empty")]
    pub credentials: Credentials,
    /// Directory of skills (`<name>/SKILL.md`) used instead of the built-in
    /// ones of the same name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills_dir: Option<String>,
    /// Names for hires whose hirer doesn't pick one, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Board {
    pub name: String,
}

impl Default for Board {
    fn default() -> Self {
        Board {
            name: "Board".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    /// Claude turns the whole firm may run per day; 0 is no limit.
    pub daily_turns: u32,
    /// Turns per worker per day; 0 is no limit.
    pub worker_daily_turns: u32,
    /// Tokens (input and output) the firm may use per day; 0 is no limit.
    pub daily_tokens: u64,
    /// Workers in a turn at the same time.
    pub max_concurrent: u32,
    /// Tokens the CEO gets at the start of a run to scope the board's task.
    /// Non-zero makes the run's budget a deal with the board: the CEO asks
    /// for a budget (`budget_request`), the board approves it, and workers
    /// spend from balances that flow down the chart. 0: no run budget.
    pub seed: u64,
    /// The security officers' share of the seed and of every approved
    /// budget, from 0 to 1.
    pub security_share: f64,
    /// Tokens a contract offer moves from the hirer to the candidate for
    /// their quoting turns.
    pub quote_fee: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            daily_turns: 400,
            worker_daily_turns: 0,
            daily_tokens: 0,
            max_concurrent: 3,
            seed: 0,
            security_share: 0.1,
            quote_fee: 30_000,
        }
    }
}

/// Bounds on what workers send, against runaway firms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Messages in one thread before it is paused.
    pub thread_messages: u32,
    /// Messages a worker may send per hour.
    pub mail_per_hour: u32,
    /// `all` and `team:` messages a worker may send per hour.
    pub fanout_per_hour: u32,
    /// Messages two workers may exchange in ten minutes before the post
    /// office stops them and tells the board: a guard against loops.
    pub pair_per_10min: u32,
    /// Messages in a run.
    pub mail_per_run: u32,
    /// Size of one published artifact, e.g. `50MiB`.
    pub artifact_size: String,
    /// Size of the shared drive of one run.
    pub drive_size: String,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            thread_messages: 60,
            mail_per_hour: 120,
            fanout_per_hour: 10,
            pair_per_10min: 30,
            mail_per_run: 20_000,
            artifact_size: "50MiB".into(),
            drive_size: "2GiB".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Role {
    /// The zapper profile workers of this role run with.
    pub profile: String,
    /// Claude model; the profile's if unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub skills: RoleSkills,
    pub avatar: Avatar,
    /// Default title of workers of this role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Host path of a script for the fake Claude, which plays it instead of
    /// running Claude Code. For tests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fake: Option<String>,
    /// Roles a worker of this role may hire, during a run with a budget.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hires: Vec<String>,
    /// Workers of this role are security officers: hired during a run,
    /// they join the officers once their background check clears.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub officer: bool,
    /// Workers of this role own the firm's services (git, the cluster, the
    /// product): they decide colleagues' access requests to them, instead
    /// of the board.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub owns_services: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoleSkills {
    pub work: String,
    pub communication: String,
    pub publishing: String,
}

impl Default for RoleSkills {
    fn default() -> Self {
        RoleSkills {
            work: String::new(),
            communication: "office-mail".into(),
            publishing: "office-publish".into(),
        }
    }
}

impl RoleSkills {
    pub fn names(&self) -> Vec<&str> {
        [
            self.communication.as_str(),
            self.work.as_str(),
            self.publishing.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Avatar {
    /// `ceo`, `product-manager`, `manager`, `engineer`, `designer`, `researcher`,
    /// `analyst`, `shield`; others draw a plain figure.
    pub figure: String,
    pub color: String,
}

impl Default for Avatar {
    fn default() -> Self {
        Avatar {
            figure: "plain".into(),
            color: "#D97757".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub name: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Security {
    /// Workers who decide access requests and watch traffic.
    pub officers: Vec<String>,
    pub digest_every: Digest,
    /// After this long without an officer's answer, the board is asked too.
    pub answer_within: String,
}

impl Default for Security {
    fn default() -> Self {
        Security {
            officers: Vec::new(),
            digest_every: Digest::default(),
            answer_within: "15m".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Digest {
    pub minutes: u32,
    pub requests: u32,
}

impl Default for Digest {
    fn default() -> Self {
        Digest {
            minutes: 10,
            requests: 200,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitService {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facility {
    /// `k3s`.
    pub kind: String,
    pub profile: String,
}

/// A static landing page about the firm, deployed on the cluster at boot at
/// the lowest routing priority, so the product replaces it on `/`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Landing {
    /// The firm's display name; default: the firm's name, capitalised, with
    /// " Inc.".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub motto: String,
}

impl Landing {
    pub fn display_name(&self, firm: &str) -> String {
        if self.name.is_empty() {
            format!("{} Inc.", title_case(firm))
        } else {
            self.name.clone()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Credentials {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kubeconfig: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub git: Vec<String>,
}

impl Credentials {
    fn is_empty(&self) -> bool {
        self.kubeconfig.is_empty() && self.git.is_empty()
    }
}

/// Addresses that aren't workers.
pub const BOARD: &str = "board";
const RESERVED: [&str; 6] = [BOARD, "all", "team", "security", "office", "post-office"];

/// Whether `name` is reserved for something other than a worker.
pub fn reserved_name(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// Parses a token amount: `2000000`, `500k`, `2M`, `1.5M`.
pub fn parse_tokens(s: &str) -> Result<u64> {
    let t = s.trim().replace(['_', ','], "");
    let (num, mult) = match t.chars().last() {
        Some('k' | 'K') => (&t[..t.len() - 1], 1e3),
        Some('m' | 'M') => (&t[..t.len() - 1], 1e6),
        Some('g' | 'G' | 'b' | 'B') => (&t[..t.len() - 1], 1e9),
        _ => (t.as_str(), 1.0),
    };
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow!("{s:?} is not a token amount like 500k or 2M"))?;
    if !n.is_finite() || n < 0.0 {
        bail!("{s:?} is not a token amount like 500k or 2M");
    }
    Ok((n * mult).round() as u64)
}

/// Parses `90s`, `15m`, `2h`, `1d`, or plain seconds.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().map_err(|_| anyhow!("duration {s:?}"))?;
    let mult = match unit.trim() {
        "" | "s" => 1,
        "m" | "min" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => bail!("duration {s:?}: unknown unit"),
    };
    Ok(Duration::from_secs(n * mult))
}

/// Whether `name` can name a worker: lowercase letters, digits and dashes.
pub fn valid_worker_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

impl Firm {
    pub fn domain(&self, firm: &str) -> String {
        if self.domain.is_empty() {
            format!("{firm}.internal")
        } else {
            self.domain.to_ascii_lowercase()
        }
    }

    pub fn worker(&self, name: &str) -> Option<&Worker> {
        self.workers.iter().find(|w| w.name == name)
    }

    pub fn role_of(&self, name: &str) -> Option<&Role> {
        self.worker(name).and_then(|w| self.roles.get(&w.role))
    }

    /// The worker's manager, `None` for the root.
    pub fn manager_of(&self, name: &str) -> Option<&str> {
        self.worker(name)?.reports_to.as_deref()
    }

    pub fn reports_of(&self, name: &str) -> Vec<&str> {
        self.workers
            .iter()
            .filter(|w| w.reports_to.as_deref() == Some(name))
            .map(|w| w.name.as_str())
            .collect()
    }

    /// The worker everyone reports to, directly or not.
    pub fn root(&self) -> Option<&str> {
        self.workers
            .iter()
            .find(|w| w.reports_to.is_none())
            .map(|w| w.name.as_str())
    }

    /// Whether `a` is above `b` in the chart (its manager, or the manager's
    /// manager, and so on). The board is above everyone.
    pub fn is_above(&self, a: &str, b: &str) -> bool {
        if a == BOARD {
            return b != BOARD;
        }
        let mut cur = self.manager_of(b);
        let mut hops = 0;
        while let Some(m) = cur {
            if m == a {
                return true;
            }
            hops += 1;
            if hops > self.workers.len() {
                return false;
            }
            cur = self.manager_of(m);
        }
        false
    }

    pub fn is_officer(&self, name: &str) -> bool {
        self.security.officers.iter().any(|o| o == name)
    }

    pub fn title_of(&self, name: &str) -> String {
        let Some(w) = self.worker(name) else {
            return if name == BOARD {
                self.board.name.clone()
            } else {
                name.to_string()
            };
        };
        w.title
            .clone()
            .or_else(|| self.roles.get(&w.role).and_then(|r| r.title.clone()))
            .unwrap_or_else(|| title_case(&w.role))
    }

    /// Checks the firm against the rest of the config.
    pub fn validate(&self, name: &str, config: &Config) -> Vec<String> {
        let mut errors = Vec::new();
        let mut err = |e: String| errors.push(format!("firm {name}: {e}"));
        if !valid_worker_name(name) {
            err("a firm name is lowercase letters, digits and dashes".into());
        }
        if crate::names::hostname(&self.domain(name)).is_err() {
            err(format!(
                "domain {:?} is not a valid host name",
                self.domain(name)
            ));
        }
        // The product may be served to the LAN (`0.0.0.0:8080`); the
        // dashboard signs in as cluster-admin, so it stays on loopback.
        for (field, port, lan) in [
            ("product_port", &self.product_port, true),
            ("dashboard_port", &self.dashboard_port, false),
        ] {
            let Some(p) = port else { continue };
            match p.parse::<std::net::SocketAddr>() {
                Ok(a) if a.ip().is_loopback() || lan => {}
                Ok(a) => err(format!("{field} must be a loopback address, not {a}")),
                Err(_) => err(format!(
                    "{field} {p:?} is not an address like 127.0.0.1:8080"
                )),
            }
        }
        if self.dashboard_port.is_some() && self.facilities.is_empty() {
            err("dashboard_port needs a cluster facility".into());
        }
        if self.budget.max_concurrent == 0 {
            err("budget.max_concurrent must be at least 1".into());
        }
        if let Err(e) = parse_duration(&self.security.answer_within) {
            err(format!("security.answer_within: {e}"));
        }
        for (field, v) in [
            ("artifact_size", &self.limits.artifact_size),
            ("drive_size", &self.limits.drive_size),
        ] {
            if crate::parse_size(v).is_err() {
                err(format!("limits.{field} {v:?} is not a size like 50MiB"));
            }
        }
        if self.workers.is_empty() {
            err("has no workers".into());
        }
        let mut names = BTreeSet::new();
        for w in &self.workers {
            if !valid_worker_name(&w.name) {
                err(format!("worker name {:?}: lowercase letters, digits and dashes, starting with a letter", w.name));
            }
            if RESERVED.contains(&w.name.as_str()) {
                err(format!("worker name {:?} is reserved", w.name));
            }
            if !names.insert(w.name.as_str()) {
                err(format!("worker {} appears twice", w.name));
            }
            if !self.roles.contains_key(&w.role) {
                err(format!("worker {}: unknown role {}", w.name, w.role));
            }
        }
        for w in &self.workers {
            if let Some(m) = &w.reports_to {
                if !names.contains(m.as_str()) {
                    err(format!("worker {} reports to unknown worker {m}", w.name));
                }
            }
        }
        let roots: Vec<&str> = self
            .workers
            .iter()
            .filter(|w| w.reports_to.is_none())
            .map(|w| w.name.as_str())
            .collect();
        if !self.workers.is_empty() && roots.len() != 1 {
            err(format!(
                "the chart needs exactly one worker who reports to no one, found {roots:?}"
            ));
        }
        // Everyone reaches the root: no cycles.
        for w in &self.workers {
            let mut seen = BTreeSet::new();
            let mut cur = Some(w.name.as_str());
            while let Some(c) = cur {
                if !seen.insert(c) {
                    err(format!("reports_to has a cycle through {c}"));
                    break;
                }
                cur = self.worker(c).and_then(|x| x.reports_to.as_deref());
            }
        }
        for o in &self.security.officers {
            if !names.contains(o.as_str()) {
                err(format!("security officer {o} is not a worker"));
            }
        }
        for (rname, role) in &self.roles {
            if role.skills.work.is_empty() {
                err(format!("role {rname}: skills.work is empty"));
            }
            for s in role.skills.names() {
                if !valid_worker_name(s) {
                    err(format!(
                        "role {rname}: skill name {s:?}: lowercase letters, digits and dashes"
                    ));
                }
            }
            match config.resolve(&role.profile) {
                Err(e) => err(format!("role {rname}: profile {}: {e:#}", role.profile)),
                Ok(p) => {
                    // Officers decide network access only; an exec can't wait
                    // minutes for them.
                    let mut layers: Vec<&crate::Policy> = p.layers.iter().map(|(_, l)| l).collect();
                    layers.push(&config.policy);
                    if layers.iter().any(|l| {
                        l.exec.iter().any(|r| r.action == Action::Ask)
                            || l.defaults.exec == Some(Action::Ask)
                    }) {
                        err(format!("role {rname}: profile {} has exec rules that ask; in a firm, exec rules are allow or deny", role.profile));
                    }
                }
            }
        }
        for (fname, f) in &self.facilities {
            if f.kind != "k3s" {
                err(format!("facility {fname}: unknown kind {} (k3s)", f.kind));
            }
            if let Err(e) = config.resolve(&f.profile) {
                err(format!("facility {fname}: profile {}: {e:#}", f.profile));
            }
        }
        if self.facilities.len() > 1 {
            err("one facility (the cluster) is supported".into());
        }
        for r in self
            .credentials
            .kubeconfig
            .iter()
            .chain(&self.credentials.git)
        {
            if !self.roles.contains_key(r) {
                err(format!("credentials: unknown role {r}"));
            }
        }
        if !self.credentials.kubeconfig.is_empty() && self.facilities.is_empty() {
            err("credentials.kubeconfig needs a cluster facility".into());
        }
        if !self.credentials.git.is_empty() && self.git.is_none() {
            err("credentials.git needs git: {}".into());
        }
        if self.product_port.is_some() && self.facilities.is_empty() {
            err("product_port needs a cluster facility".into());
        }
        if self.landing.is_some() && self.facilities.is_empty() {
            err("landing needs a cluster facility".into());
        }
        if let Some(l) = &self.landing {
            if l.name.len() > 200 || l.motto.len() > 500 {
                err("landing: keep the name under 200 and the motto under 500 characters".into());
            }
        }
        for (rname, role) in &self.roles {
            for h in &role.hires {
                if !self.roles.contains_key(h) {
                    err(format!("role {rname}: hires unknown role {h}"));
                }
            }
        }
        if !(0.0..=1.0).contains(&self.budget.security_share) {
            err("budget.security_share is a fraction from 0 to 1".into());
        }
        let hires = self.roles.values().any(|r| !r.hires.is_empty());
        if hires && self.budget.seed == 0 {
            err("roles that hire need a run budget: set budget.seed".into());
        }
        let mut pool = BTreeSet::new();
        for n in &self.names {
            if !valid_worker_name(n) || RESERVED.contains(&n.as_str()) {
                err(format!("names: {n:?} can't name a worker"));
            }
            if !pool.insert(n.as_str()) || names.contains(n.as_str()) {
                err(format!("names: {n} is taken"));
            }
        }
        errors
    }
}

pub fn title_case(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "
profiles:
  w: { agent: { kind: claude-code } }
  asks: { policy: { exec: [{ action: ask, path: ssh }] } }
firms:
  acme:
    roles:
      ceo: { profile: w, skills: { work: ceo } }
      eng: { profile: w, skills: { work: software-engineer } }
    workers:
      - { name: ceo, role: ceo }
      - { name: cto, role: eng, reports_to: ceo, title: CTO }
      - { name: ada, role: eng, reports_to: cto }
    security: { officers: [] }
";

    fn firm(yaml: &str) -> Result<Config> {
        Config::from_yaml(yaml)
    }

    #[test]
    fn chart() {
        let c = firm(BASE).unwrap();
        let f = &c.firms["acme"];
        assert_eq!(f.root(), Some("ceo"));
        assert_eq!(f.manager_of("ada"), Some("cto"));
        assert_eq!(f.reports_of("ceo"), vec!["cto"]);
        assert!(f.is_above("ceo", "ada"));
        assert!(f.is_above("cto", "ada"));
        assert!(!f.is_above("ada", "cto"));
        assert!(!f.is_above("ada", "ada"));
        assert!(f.is_above(BOARD, "ceo"));
        assert_eq!(f.title_of("cto"), "CTO");
        assert_eq!(f.title_of("ada"), "Eng");
        assert_eq!(f.domain("acme"), "acme.internal");
    }

    #[test]
    fn rejects_bad_charts() {
        let two_roots = BASE.replace("reports_to: cto", "");
        assert!(firm(&two_roots)
            .unwrap_err()
            .to_string()
            .contains("exactly one worker"));
        let cycle = BASE.replace(
            "{ name: ceo, role: ceo }",
            "{ name: ceo, role: ceo, reports_to: ada }",
        );
        let e = firm(&cycle).unwrap_err().to_string();
        assert!(e.contains("cycle") || e.contains("exactly one"), "{e}");
        let dup = BASE.replace("name: ada", "name: cto");
        assert!(firm(&dup).unwrap_err().to_string().contains("twice"));
        let reserved = BASE.replace("name: ada", "name: board");
        assert!(firm(&reserved)
            .unwrap_err()
            .to_string()
            .contains("reserved"));
        let unknown = BASE.replace("reports_to: cto", "reports_to: nobody");
        assert!(firm(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown worker nobody"));
        let role = BASE.replace(
            "role: eng, reports_to: cto",
            "role: intern, reports_to: cto",
        );
        assert!(firm(&role)
            .unwrap_err()
            .to_string()
            .contains("unknown role intern"));
    }

    #[test]
    fn exec_ask_is_refused_in_firm_profiles() {
        let c = BASE.replace("eng: { profile: w", "eng: { profile: asks");
        assert!(firm(&c)
            .unwrap_err()
            .to_string()
            .contains("exec rules that ask"));
    }

    #[test]
    fn ports() {
        let with = |ports: &str| {
            BASE.replace("    security: { officers: [] }", &format!("    security: {{ officers: [] }}\n    facilities: {{ cluster: {{ kind: k3s, profile: w }} }}\n    {ports}"))
        };
        assert!(firm(&with("product_port: 127.0.0.1:8080")).is_ok());
        // The product may be served to the LAN, the dashboard never.
        assert!(firm(&with("product_port: 0.0.0.0:8080")).is_ok());
        let e = firm(&with("dashboard_port: 0.0.0.0:8081"))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("dashboard_port must be a loopback address"),
            "{e}"
        );
        assert!(firm(&with("product_port: lan:8080"))
            .unwrap_err()
            .to_string()
            .contains("is not an address"));
    }

    #[test]
    fn hiring_config() {
        let with = |extra: &str| {
            BASE.replace(
                "      eng: { profile: w, skills: { work: software-engineer } }",
                &format!(
                    "      eng: {{ profile: w, skills: {{ work: software-engineer }}{extra} }}"
                ),
            )
        };
        let e = firm(&with(", hires: [intern]")).unwrap_err().to_string();
        assert!(e.contains("hires unknown role intern"), "{e}");
        let e = firm(&with(", hires: [eng]")).unwrap_err().to_string();
        assert!(e.contains("need a run budget"), "{e}");
        let ok = with(", hires: [eng]").replace("    security: { officers: [] }", "    security: { officers: [] }\n    budget: { seed: 2000000 }\n    names: [grace, ken]");
        let c = firm(&ok).unwrap();
        assert_eq!(c.firms["acme"].roles["eng"].hires, vec!["eng"]);
        assert_eq!(c.firms["acme"].budget.security_share, 0.1);
        let taken = ok.replace("names: [grace, ken]", "names: [grace, ada]");
        assert!(firm(&taken)
            .unwrap_err()
            .to_string()
            .contains("names: ada is taken"));
    }

    #[test]
    fn token_amounts() {
        assert_eq!(parse_tokens("2M").unwrap(), 2_000_000);
        assert_eq!(parse_tokens("1.5M").unwrap(), 1_500_000);
        assert_eq!(parse_tokens("500k").unwrap(), 500_000);
        assert_eq!(parse_tokens("1_000").unwrap(), 1000);
        assert!(parse_tokens("lots").is_err());
        assert!(parse_tokens("-1M").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
        assert!(parse_duration("soon").is_err());
    }
}
