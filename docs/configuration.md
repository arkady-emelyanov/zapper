[← README](../README.md) · [Docs](README.md) · **Configuration**

# Configuration

## Overview

zapper reads one YAML file, by default `~/.config/zapper/zapper.yaml`; `--config` or `$ZAPPER_CONFIG` selects another. `zapper init` copies the commented [example](../config/zapper.example.yaml) there with mode 0600, and leaves an existing file as it is.

## Running and startup config

The daemon loads the file at start as the running config. API changes edit the running config, as on a router:

| Command | Effect |
|---|---|
| `zapper config show` | Print the running config, with secret values redacted |
| `zapper config startup` | Print the file as loaded |
| `zapper config diff` | Show what `save` would write |
| `zapper config save` | Write the running config to the file (0600), keeping the previous file as `.yaml.bak` |
| `zapper config reload` | Load the file again, discarding unsaved changes |
| `zapper config edit` | Edit the running config in `$EDITOR` |
| `zapper config apply FILE` | Replace the running config with a file |

Each change is validated as a whole before it applies. A change with an unknown profile, an `extends` cycle, a duplicate rule id, a bad glob or an unknown secret is rejected, and the running config stays as it was. When a config containing `<redacted>` inline secret values is applied, those secrets keep their current values. `save` writes plain YAML, so comments in the file are lost.

## The file

```
daemon:     paths, UI address, limits, private networks          (below)
secrets:    name → source, mode and hosts                         (policy.md: Secrets)
policy:     the global layer                                     (policy.md)
profiles:   name → profile
```

### daemon

| Key | Default | Meaning |
|---|---|---|
| `firecracker` | `~/.local/share/zapper/bin/firecracker` | VMM binary (`make deps`) |
| `agent` | next to `zapper` | Static `zapper-agent` binary |
| `images` | `~/.local/share/zapper/images` | Root filesystem images, `<name>.ext4` |
| `kernels` | `~/.local/share/zapper/kernels` | Guest kernels |
| `state` | `~/.local/state/zapper` | Session directories with audit logs, recordings, and disks while running |
| `ui` | `127.0.0.1:7878` | Address of the UI (state and approvals), which must be loopback; empty turns the UI off |
| `ask_timeout_secs` | `60` | How long an `ask` waits before it denies |
| `max_sessions` | `16` | Sessions starting or running at the same time |
| `private_networks` | `[]` | CIDRs of private networks sessions may reach ([network](network.md#address-checks)) |

### profiles

```
profiles:
  claude:
    extends: default          # engine and agent merge key by key; mounts and setup add up
    description: …
    engine: { image: claude, kernel: vmlinux-6.1, vcpus: 2, memory_mib: 4096, disk: 4GiB, ttl_secs: 0, network: nic, boot_args: "" }
    agent:  { … }             # below
    mounts: [{ host: "{workdir}", guest: /work, mode: rw, extra: 1GiB }]
    setup:  ["apt-get install -y jq"]    # run as root at boot, under this profile's policy
    policy: { net: […], exec: […], fs: […], defaults: { net: ask } }
```

`engine.network` is `nic` (the default) or `vsock`; [network](network.md) describes both.

`mounts` are copied in at boot. `{workdir}` is the directory `zapper run` was started in, or the one given with `-C`. A `rw` disk gets `extra` free space, 1 GiB by default. Changes stay in the session's disk until you copy them out with `zapper cp ID:/work ./out`.

### agent

| Key | Default | Meaning |
|---|---|---|
| `kind` | `claude-code` | `claude-code` also writes Claude Code's files (below); `generic` writes `env` and `files` |
| `command` | `[claude]` | What `zapper run` and `zapper attach` start |
| `user` | `agent` | The user the command runs as, created with uid 1000 |
| `root` | `false` | Run as root; this gives up the in-guest guardrails ([security](security.md#what-is-enforced-where)) |
| `workdir` | `/work` | Working directory |
| `env` | | Environment. `{{secret:NAME}}` becomes a placeholder for `proxy` secrets and the value for `guest` secrets |
| `files` | | Guest path (`~` is the home directory) → content, with `{{secret:NAME}}` as in `env`. Files with a `guest` secret are created 0600 |
| `model` | | Claude Code: `model` in `~/.claude/settings.json` |
| `permission_mode` | | Claude Code: `permissions.defaultMode` in the managed settings file, `/etc/claude-code/managed-settings.json`, which is root-owned. Set there, Claude Code doesn't ask at each start whether to switch to auto mode, and the workload can't change it. With `bypassPermissions`, the file also sets `skipDangerousModePermissionPrompt` |
| `settings` | | Claude Code: merged into `~/.claude/settings.json` |
| `mcp_servers` | | Claude Code: `mcpServers` in `~/.claude.json` |
| `claude_md` | | Claude Code: `~/.claude/CLAUDE.md` |

Every session's environment also has:

- `NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE` and `REQUESTS_CA_BUNDLE`, so tools trust the session CA
- `ZAPPER_SESSION`, the session id
- in vsock mode, `HTTP_PROXY` and `HTTPS_PROXY` set to `http://127.0.0.1:3128`

For `claude-code`, zapper also sets `DISABLE_AUTOUPDATER=1` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, and marks onboarding, the folder trust dialog and the bypass-permissions prompt as accepted.

## Verification

```
zapper check                     # loads and validates the file
zapper profile show claude       # the profile and its resolved form
```

## Limitations

- `save` drops comments from the file; keep commentary in the example or the docs.
- A running session keeps the engine and agent it started with. Rule changes apply to it at once; engine and agent changes apply to sessions started afterwards.

## References

- Sources: `crates/zapper-policy/src/config.rs`, `crates/zapper/src/daemon/{mod,spec}.rs`
