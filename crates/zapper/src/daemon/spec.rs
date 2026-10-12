//! Renders a profile into the spec the guest sets itself up from.
//!
//! `proxy` secrets appear here only as placeholders; `guest` secrets as their
//! values.

use std::collections::BTreeMap;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use zapper_policy::proto::{GuestDisk, GuestFile, GuestSpec};
use zapper_policy::{merge, placeholder, render, secret_refs, Config, FsRule, NetworkMode, ResolvedProfile, Scope, SecretMode};

const CA_PATH: &str = "/usr/local/share/ca-certificates/zapper.crt";
const PROXY: &str = "http://127.0.0.1:3128";

/// Renders `{{secret:NAME}}`: placeholders for proxy secrets, values for
/// guest secrets.
fn render_secrets(config: &Config, template: &str) -> Result<String> {
    let mut err = None;
    let out = render(template, |name| match config.secrets.get(name) {
        Some(s) if s.mode == SecretMode::Guest => s.source.resolve().unwrap_or_else(|e| {
            err.get_or_insert(e.context(format!("secret {name}")));
            String::new()
        }),
        Some(_) => placeholder(name),
        None => {
            err.get_or_insert(anyhow!("unknown secret {name}"));
            String::new()
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

pub fn build(config: &Config, profile: &ResolvedProfile, session: &str, ca_pem: &str, disks: Vec<GuestDisk>) -> Result<GuestSpec> {
    let agent = &profile.agent;
    let home = format!("/home/{}", agent.user);
    let nic = profile.engine.network == NetworkMode::Nic;
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    if !nic {
        // In vsock mode, programs reach the network through the forwarder.
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env.insert(k.into(), PROXY.into());
        }
        env.insert("NO_PROXY".into(), "localhost,127.0.0.1".into());
        env.insert("no_proxy".into(), "localhost,127.0.0.1".into());
    }
    for (k, v) in [
        ("NODE_EXTRA_CA_CERTS", CA_PATH),
        ("SSL_CERT_FILE", "/etc/ssl/certs/ca-certificates.crt"),
        ("REQUESTS_CA_BUNDLE", "/etc/ssl/certs/ca-certificates.crt"),
        ("ZAPPER_SESSION", session),
    ] {
        env.insert(k.into(), v.into());
    }
    let mut files = Vec::new();
    for (k, v) in &agent.env {
        env.insert(k.clone(), render_secrets(config, v).with_context(|| format!("agent.env.{k}"))?);
    }
    if agent.kind == "claude-code" {
        env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
        env.insert("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into());
        files.extend(claude_files(profile, &home, env.get("ANTHROPIC_API_KEY")));
    }
    for (path, content) in &agent.files {
        let path = match path.strip_prefix("~/") {
            Some(rest) => format!("{home}/{rest}"),
            None => path.clone(),
        };
        let user_owned = path.starts_with(&home);
        // Files holding guest secrets are private to the user.
        let holds_secret = secret_refs(content).iter().any(|n| config.secrets.get(n).is_some_and(|s| s.mode == SecretMode::Guest));
        let mode = if holds_secret { 0o600 } else { 0o644 };
        let content = render_secrets(config, content).with_context(|| format!("agent.files.{path}"))?;
        files.push(GuestFile { path, content, mode, user_owned });
    }
    // All fs rules, most specific layer first; the guest applies them all.
    let mut fs: Vec<FsRule> = Vec::new();
    for (_, p) in &profile.layers {
        fs.extend(p.fs.iter().cloned());
    }
    fs.extend(config.policy.fs.iter().cloned());
    Ok(GuestSpec {
        session: session.into(),
        nic,
        hostname: format!("zapper-{session}"),
        user: agent.user.clone(),
        root: agent.root,
        home,
        workdir: agent.workdir.clone(),
        env,
        disks,
        files,
        fs,
        setup: profile.setup.clone(),
        ca_pem: ca_pem.into(),
        supervise_exec: true,
    })
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
    if let Some(mode) = &agent.permission_mode {
        merge(&mut settings, &json!({ "permissions": { "defaultMode": mode } }));
    }
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
        let tail: String = key.chars().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect();
        merge(&mut state, &json!({ "customApiKeyResponses": { "approved": [tail], "rejected": [] } }));
    }
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap();
    let mut files = vec![
        GuestFile { path: format!("{home}/.claude/settings.json"), content: pretty(&settings), mode: 0o644, user_owned: true },
        GuestFile { path: format!("{home}/.claude.json"), content: pretty(&state), mode: 0o600, user_owned: true },
    ];
    if let Some(md) = &agent.claude_md {
        files.push(GuestFile { path: format!("{home}/.claude/CLAUDE.md"), content: md.clone(), mode: 0o644, user_owned: true });
    }
    files
}

/// Policy layers of a session in evaluation order.
pub fn layers_of<'a>(layers: &'a [(Scope, zapper_policy::Policy)]) -> Vec<(Scope, &'a zapper_policy::Policy)> {
    layers.iter().map(|(s, p)| (s.clone(), p)).collect()
}
