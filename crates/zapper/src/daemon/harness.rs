//! Agent harnesses: the coding agent a session runs. A harness says where
//! and how the agent reads its configuration, and, for office workers, how
//! to hold a conversation with it headless. The rest of zapper only sees
//! these interfaces: another harness (pi, opencode) is one more
//! implementation and a name in `agent.kind` (zapper_policy::AGENT_KINDS).
//!
//! - `claude-code`: Claude Code (`claude -p`, stream-json in and out).
//! - `generic`: any command; zapper writes only `agent.files` and `env`.
//!
//! What harnesses have in common is in the profile's agent spec, in neutral
//! terms (instructions, a skills directory, MCP servers, a turn-end command,
//! model, settings, files and directories for the config directory); each
//! harness writes it in its own place and format. What differs: the config
//! directory (`~/.claude`, `~/.pi/agent`, `~/.config/opencode`), the
//! instructions file (`CLAUDE.md`, `AGENTS.md`), the MCP config's shape, and
//! the protocol (Claude Code's stream-json, pi's `--mode rpc`, opencode's
//! ACP over stdio), which `Conversation` hides.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use zapper_policy::proto::GuestFile;
use zapper_policy::{merge, AgentSpec, Mount, MountMode, ResolvedProfile};

/// The coding agent a session runs.
pub trait Harness: Send + Sync {
    /// Its name in `agent.kind`.
    fn kind(&self) -> &'static str;
    /// Where the agent reads its configuration in the guest; the agent
    /// spec's `config_files` and `config_mounts` are relative to it.
    fn config_dir(&self, home: &str) -> String;
    /// Where it reads skills (`<name>/SKILL.md` directories).
    fn skills_dir(&self, home: &str) -> String {
        format!("{}/skills", self.config_dir(home))
    }
    /// Files and environment the agent needs, from the neutral agent spec:
    /// its settings, instructions, MCP servers, turn-end command. `env`
    /// already holds the profile's variables.
    fn guest(
        &self,
        profile: &ResolvedProfile,
        home: &str,
        env: &mut BTreeMap<String, String>,
        files: &mut Vec<GuestFile>,
    );
    /// Host directories to mount: the skills directory and `config_mounts`,
    /// read-only unless a mount says otherwise.
    fn mounts(&self, agent: &AgentSpec, home: &str) -> Vec<Mount> {
        let mut v = Vec::new();
        if let Some(dir) = &agent.skills {
            v.push(Mount {
                host: dir.clone(),
                guest: self.skills_dir(home),
                mode: MountMode::Ro,
                extra: None,
            });
        }
        for m in &agent.config_mounts {
            v.push(Mount {
                guest: format!(
                    "{}/{}",
                    self.config_dir(home),
                    m.guest.trim_start_matches('/')
                ),
                ..m.clone()
            });
        }
        v
    }
    /// How to hold a conversation with it as an office worker, if it can.
    fn driver(&self) -> Option<&dyn Driver>;
}

/// Starts conversations: one long one per office worker.
pub trait Driver: Send + Sync {
    /// A conversation, continuing `resume` (an id from `Event::Session`).
    fn conversation(&self, resume: Option<&str>) -> Box<dyn Conversation>;
}

/// One running agent process, spoken to in lines on its stdin and heard in
/// JSON lines on its stdout. Stateful: protocols with requests and replies
/// keep their ids and session here.
pub trait Conversation: Send {
    /// The command line.
    fn argv(&self) -> Vec<String>;
    /// Lines to send first (a protocol's handshake); none for Claude Code.
    fn opening(&mut self) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// The line that gives the agent a prompt (a wake), with its newline.
    fn prompt(&mut self, text: &str) -> Vec<u8>;
    /// What an output line means.
    fn read(&mut self, line: &Value) -> Vec<Event>;
    /// Whether `steer` hands messages over.
    fn steers(&self) -> bool {
        false
    }
    /// A line that hands the agent a message while it works, for harnesses
    /// that take one natively (pi's `follow_up`, opencode's `queue`
    /// delivery); `None` where the turn-end command does it (Claude Code).
    fn steer(&mut self, _text: &str) -> Option<Vec<u8>> {
        None
    }
}

/// Something the foreman needs from the agent's output.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// The conversation's id, for resuming it after the process restarts.
    Session(String),
    /// What the agent is doing now: `Edit pricing.md`.
    Activity(String),
    /// A turn ended.
    TurnEnd(TurnEnd),
    /// A line to write back (a reply the protocol expects; Claude Code's
    /// stream-json has none, opencode's ACP does).
    #[allow(dead_code)]
    Send(Vec<u8>),
}

#[derive(Debug, PartialEq)]
pub struct TurnEnd {
    /// Tokens the turn used, against budgets: input, output and cache
    /// writes (cache reads are not counted).
    pub tokens: u64,
    pub outcome: Outcome,
    /// For the journal's `turn end` record: usage and the like.
    pub detail: Value,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done,
    /// The provider's limits: try the same wake again later.
    RateLimited,
    /// The turn failed before the agent could work (a refused token, an
    /// outage): what it was handed must not be lost.
    Failed(String),
}

/// The harness of an `agent.kind`.
pub fn get(kind: &str) -> Option<&'static dyn Harness> {
    match kind {
        "claude-code" => Some(&ClaudeCode),
        "generic" => Some(&Generic),
        _ => None,
    }
}

/// Any command; nothing to set up beyond `agent.files` and `env`.
pub struct Generic;

impl Harness for Generic {
    fn kind(&self) -> &'static str {
        "generic"
    }
    fn config_dir(&self, home: &str) -> String {
        home.into()
    }
    fn guest(
        &self,
        _: &ResolvedProfile,
        _: &str,
        _: &mut BTreeMap<String, String>,
        _: &mut Vec<GuestFile>,
    ) {
    }
    fn mounts(&self, _: &AgentSpec, _: &str) -> Vec<Mount> {
        Vec::new()
    }
    fn driver(&self) -> Option<&dyn Driver> {
        None
    }
}

/// Claude Code.
pub struct ClaudeCode;

impl Harness for ClaudeCode {
    fn kind(&self) -> &'static str {
        "claude-code"
    }

    fn config_dir(&self, home: &str) -> String {
        format!("{home}/.claude")
    }

    fn guest(
        &self,
        profile: &ResolvedProfile,
        home: &str,
        env: &mut BTreeMap<String, String>,
        files: &mut Vec<GuestFile>,
    ) {
        env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
        env.insert(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "1".into(),
        );
        files.extend(claude_files(profile, home, env.get("ANTHROPIC_API_KEY")));
    }

    fn driver(&self) -> Option<&dyn Driver> {
        Some(self)
    }
}

impl Driver for ClaudeCode {
    fn conversation(&self, resume: Option<&str>) -> Box<dyn Conversation> {
        Box::new(ClaudeConversation {
            resume: resume.map(str::to_string),
        })
    }
}

/// `claude -p` with stream-json in and out: a user message per prompt, a
/// `result` event per turn.
struct ClaudeConversation {
    resume: Option<String>,
}

impl Conversation for ClaudeConversation {
    fn argv(&self) -> Vec<String> {
        let mut v: Vec<String> = [
            "claude",
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if let Some(id) = &self.resume {
            v.push("--resume".into());
            v.push(id.clone());
        }
        v
    }

    fn prompt(&mut self, text: &str) -> Vec<u8> {
        let msg = json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": text }] } });
        let mut line = serde_json::to_vec(&msg).expect("JSON");
        line.push(b'\n');
        line
    }

    fn read(&mut self, v: &Value) -> Vec<Event> {
        let mut out = Vec::new();
        if let Some(id) = v["session_id"].as_str() {
            out.push(Event::Session(id.into()));
        }
        match v["type"].as_str() {
            Some("assistant") => out.extend(claude_activity(v).map(Event::Activity)),
            Some("result") => {
                let outcome = if claude_rate_limited(v) {
                    Outcome::RateLimited
                } else if let Some(why) = claude_api_failure(v) {
                    Outcome::Failed(why)
                } else {
                    Outcome::Done
                };
                out.push(Event::TurnEnd(TurnEnd {
                    tokens: claude_tokens(v),
                    outcome,
                    detail: json!({ "usage": v["usage"], "num_turns": v["num_turns"], "is_error": v["is_error"] }),
                }));
            }
            _ => {}
        }
        out
    }
}

/// What Claude is doing, from a tool call: `Edit pricing.md`.
fn claude_activity(event: &Value) -> Option<String> {
    let content = event["message"]["content"].as_array()?;
    let tool = content.iter().rev().find(|c| c["type"] == "tool_use")?;
    let name = tool["name"].as_str().unwrap_or("tool");
    let name = name.strip_prefix("mcp__office__").unwrap_or(name);
    let input = &tool["input"];
    let arg = [
        "file_path",
        "path",
        "command",
        "pattern",
        "url",
        "subject",
        "query",
        "description",
    ]
    .iter()
    .find_map(|k| input[*k].as_str())
    .map(|s| {
        let s = s.lines().next().unwrap_or("");
        let s = s
            .rsplit('/')
            .next()
            .filter(|_| !s.contains(' '))
            .unwrap_or(s);
        s.chars().take(60).collect::<String>()
    })
    .unwrap_or_default();
    Some(format!("{name} {arg}").trim().to_string())
}

/// Tokens of a turn: input, output and cache writes.
fn claude_tokens(result: &Value) -> u64 {
    let u = &result["usage"];
    [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
    ]
    .iter()
    .map(|k| u[*k].as_u64().unwrap_or(0))
    .sum()
}

/// A turn that failed before Claude could work: the API refused it (bad
/// token, overload).
fn claude_api_failure(result: &Value) -> Option<String> {
    if result["is_error"] != json!(true) {
        return None;
    }
    let text = result["result"].as_str().unwrap_or("");
    (text.contains("API Error") || text.contains("Failed to authenticate"))
        .then(|| text.chars().take(200).collect())
}

fn claude_rate_limited(result: &Value) -> bool {
    if result["is_error"] != json!(true) {
        return false;
    }
    let text = result["result"].as_str().unwrap_or("").to_ascii_lowercase();
    text.contains("rate limit")
        || text.contains("rate_limit")
        || text.contains("usage limit")
        || text.contains("429")
        || text.contains("overloaded")
}

fn claude_files(profile: &ResolvedProfile, home: &str, api_key: Option<&String>) -> Vec<GuestFile> {
    let agent = &profile.agent;
    let mut settings = json!({});
    if let Some(s) = &agent.settings {
        merge(&mut settings, s);
    }
    if let Some(m) = &agent.model {
        merge(&mut settings, &json!({ "model": m }));
    }
    // The turn-end command as a Stop hook: it blocks the stop with the
    // reason it prints.
    if let Some(cmd) = &agent.turn_end {
        merge(
            &mut settings,
            &json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": cmd, "timeout": 30 }] }] } }),
        );
    }
    // The permission mode goes in managed settings, not ~/.claude/settings.json:
    // Claude Code asks whether to switch to auto mode when the user settings
    // file sets another mode and no other file does, and each session starts
    // with a fresh home, so it would ask every time. The managed file is also
    // root-owned, so the workload can't change it.
    // https://code.claude.com/docs/en/permission-modes#which-mode-a-session-starts-in
    let managed = agent.permission_mode.as_ref().map(|mode| {
        let mut m = json!({ "permissions": { "defaultMode": mode } });
        if mode == "bypassPermissions" {
            merge(
                &mut m,
                &json!({ "skipDangerousModePermissionPrompt": true }),
            );
        }
        m
    });
    let mut state = json!({
        "hasCompletedOnboarding": true,
        "bypassPermissionsModeAccepted": true,
        "projects": { agent.workdir.clone(): { "hasTrustDialogAccepted": true, "hasCompletedProjectOnboarding": true } },
    });
    if let Some(m) = &agent.mcp_servers {
        merge(&mut state, &json!({ "mcpServers": m }));
    }
    // Pre-approve the API key (placeholder or value) so Claude Code does not
    // ask about it.
    if let Some(key) = api_key {
        let tail: String = key
            .chars()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        merge(
            &mut state,
            &json!({ "customApiKeyResponses": { "approved": [tail], "rejected": [] } }),
        );
    }
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap();
    let mut files = vec![
        GuestFile {
            path: format!("{home}/.claude/settings.json"),
            content: pretty(&settings),
            mode: 0o644,
            user_owned: true,
        },
        GuestFile {
            path: format!("{home}/.claude.json"),
            content: pretty(&state),
            mode: 0o600,
            user_owned: true,
        },
    ];
    if let Some(md) = &agent.instructions {
        files.push(GuestFile {
            path: format!("{home}/.claude/CLAUDE.md"),
            content: md.clone(),
            mode: 0o644,
            user_owned: true,
        });
    }
    if let Some(m) = managed {
        files.push(GuestFile {
            path: MANAGED_SETTINGS.into(),
            content: pretty(&m),
            mode: 0o644,
            user_owned: false,
        });
    }
    files
}

/// Claude Code's managed settings file on Linux.
/// https://code.claude.com/docs/en/managed-settings
pub(crate) const MANAGED_SETTINGS: &str = "/etc/claude-code/managed-settings.json";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        for k in zapper_policy::AGENT_KINDS {
            assert_eq!(get(k).unwrap().kind(), *k);
        }
        assert!(get("pi").is_none());
        assert!(get("claude-code").unwrap().driver().is_some());
        assert!(
            get("generic").unwrap().driver().is_none(),
            "a generic command can't be driven as a worker"
        );
    }

    #[test]
    fn neutral_config_lands_where_each_harness_reads_it() {
        let mut agent = zapper_policy::AgentSpec {
            instructions: Some("Be brief.".into()),
            skills: Some("/host/skills".into()),
            turn_end: Some("hook".into()),
            ..Default::default()
        };
        agent.config_mounts.push(Mount {
            host: "/host/agents".into(),
            guest: "agents".into(),
            mode: MountMode::Ro,
            extra: None,
        });
        let mounts = ClaudeCode.mounts(&agent, "/home/a");
        assert_eq!(
            mounts.iter().map(|m| m.guest.as_str()).collect::<Vec<_>>(),
            vec!["/home/a/.claude/skills", "/home/a/.claude/agents"]
        );
        assert!(mounts.iter().all(|m| m.mode == MountMode::Ro));
        let profile = zapper_policy::ResolvedProfile {
            name: "p".into(),
            engine: Default::default(),
            agent,
            mounts: vec![],
            setup: vec![],
            layers: vec![],
        };
        let (mut env, mut files) = (BTreeMap::new(), Vec::new());
        ClaudeCode.guest(&profile, "/home/a", &mut env, &mut files);
        let file = |p: &str| {
            files
                .iter()
                .find(|f| f.path == p)
                .map(|f| f.content.clone())
        };
        assert_eq!(
            file("/home/a/.claude/CLAUDE.md").as_deref(),
            Some("Be brief.")
        );
        assert!(
            file("/home/a/.claude/settings.json")
                .unwrap()
                .contains("\"Stop\""),
            "the turn-end command is a Stop hook"
        );
        assert_eq!(ClaudeCode.config_dir("/home/a"), "/home/a/.claude");
        assert!(
            Generic.mounts(&profile.agent, "/home/a").is_empty(),
            "generic sets nothing up"
        );
    }

    #[test]
    fn claude_output() {
        let mut d = ClaudeCode.driver().unwrap().conversation(Some("s-0"));
        assert_eq!(d.argv().last().unwrap(), "s-0");
        let e = json!({ "type": "assistant", "session_id": "s-1", "message": { "content": [{ "type": "tool_use", "name": "Edit", "input": { "file_path": "/work/pricing.md" } }] } });
        assert_eq!(
            d.read(&e),
            vec![
                Event::Session("s-1".into()),
                Event::Activity("Edit pricing.md".into())
            ]
        );
        let e = json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "mcp__office__mail_send", "input": { "subject": "Plan" } }] } });
        assert_eq!(d.read(&e), vec![Event::Activity("mail_send Plan".into())]);
        let e = json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "hi" }] } });
        assert!(d.read(&e).is_empty());
        let mut end = |v: Value| match d.read(&v).pop() {
            Some(Event::TurnEnd(t)) => t,
            other => panic!("{other:?}"),
        };
        let t = end(
            json!({ "type": "result", "is_error": false, "usage": { "input_tokens": 100, "output_tokens": 10, "cache_creation_input_tokens": 5, "cache_read_input_tokens": 1000 } }),
        );
        assert_eq!((t.tokens, t.outcome), (115, Outcome::Done));
        assert!(matches!(end(json!({ "type": "result", "is_error": true, "result": "Failed to authenticate. API Error: 401 Invalid bearer token" })).outcome, Outcome::Failed(_)));
        assert_eq!(end(json!({ "type": "result", "is_error": true, "result": "API Error: 429 rate_limit_error" })).outcome, Outcome::RateLimited);
        assert_eq!(
            end(json!({ "type": "result", "is_error": true, "result": "reached max turns" }))
                .outcome,
            Outcome::Done
        );
        assert_eq!(end(json!({ "type": "result", "is_error": false, "result": "rate limit, API Error, in prose" })).outcome, Outcome::Done);
        assert!(d.opening().is_empty() && d.steer("x").is_none());
        let line = d.prompt("hello");
        assert_eq!(
            serde_json::from_slice::<Value>(&line).unwrap()["message"]["content"][0]["text"],
            "hello"
        );
        assert!(line.ends_with(b"\n"));
    }
}
