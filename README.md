# zapper

zapper runs programs in Firecracker microVMs and controls what they can reach. Typical programs are a coding agent, an interview candidate's shell, or a student's language sandbox. Each session gets a disposable Linux VM with a network card. zapper captures the VM's traffic on the host, checks each connection, DNS query and exec against layered rules, and records it all. Tokens can be added to requests by the host, so the program uses them without holding them, or given to the program when it has to hold them itself.

> [!TIP]
> If you use a coding agent, point it at [AGENTS.md](AGENTS.md) and it will set zapper up. Operating a running daemon is covered by the [zapper skill](skills/zapper/SKILL.md).

## Use cases

- **Coding agents.** Claude Code runs with `bypassPermissions` inside a VM. The host adds its API token to requests to `api.anthropic.com`, so the agent can use the token but can't read it. You choose the hosts it reaches, for example read-only GitHub, and you see every command it runs.
- **Interviews.** A candidate gets a shell for two hours, with access to the lab services you allow and the credentials you issue. Afterwards you can replay their terminal.
- **Study sandboxes.** A Python or Rust environment that reaches its package index, with a time limit.

zapper is a poor fit for GPU work inside the VM, because Firecracker can't pass devices through. Reach a GPU cluster through its API instead. It also doesn't run desktops, Windows guests or long-lived servers.

## How it works

```
 you ── zapper CLI ──unix socket──▶ zapper daemon ──▶ Firecracker microVM (one per session)
          ▲ REST API: rules, sessions,   │  ▲            ┌──────────────────────────────────┐
          │ approvals, config save/diff  │  │ vsock      │ zapper-agent (PID 1)             │
 browser ─┘ read-only UI (localhost)     │  └────────────│  • fetches spec, sets up guest   │
                                         │               │  • terminals over vsock          │
            egress proxy ◀── netns capture ◀─── eth0 ────│  • asks host before every exec   │
            • rules per connection/request               │  • reports process exits         │
            • injects secrets, TLS inspection            │ workload as uid 1000             │
            • policy DNS, address checks                 └──────────────────────────────────┘
                 │
                 ▼ internet / lab
```

Rules are layered: session, then profile, then the profiles it extends, then global. Within a layer the first matching rule wins. Rules marked `final` in the global layer win over everything else. Configuration works like a router's: API changes apply to the running config at once, `zapper config save` writes them to the startup file, and `zapper config diff` shows what hasn't been saved.

More detail: [architecture](docs/architecture.md), [network](docs/network.md), [policy](docs/policy.md), [security model](docs/security.md).

## Quickstart

On a Linux host with KVM:

```
make deps                     # Firecracker and a guest kernel into ~/.local/share/zapper (~1 min)
make install                  # zapper and zapper-agent into ~/.local/bin
zapper init                   # ~/.config/zapper/zapper.yaml from the example
make image                    # base Ubuntu image (~2 min); `make image IMAGE=claude` for Claude Code
zapper check                  # each line "ok", or the command that fixes it
zapper serve                  # the daemon; prints the read-only UI URL
```

In another terminal:

```
cd ~/some/project
zapper run                    # a shell in a VM on a copy of this directory, ready in about a second
zapper run -p claude          # Claude Code, with its token added by the host
zapper events                 # live connections, DNS queries, execs, exits and approvals
zapper rule add profile:claude '{kind: net, action: allow, host: pypi.org, methods: [GET]}'
zapper config diff            # the unsaved change; `zapper config save` keeps it
```

[docs/commands.md](docs/commands.md) lists every command, and [docs/README.md](docs/README.md) indexes the documentation.

## Status

zapper is early software that works end to end. `make test-e2e` boots real VMs and checks each control from inside the guest, including attempts to get around the controls as root. [docs/security.md](docs/security.md) lists the known limitations.

## Next

1. **Network rate and volume limits** per session: bandwidth and packet rate on the NIC, using Firecracker's rate limiter, and byte counts per destination and per session in the proxy, with an event when a limit is hit.
2. **VMM filesystem confinement:** run Firecracker with only its session directory visible, through a mount namespace or the jailer.
3. **UDP egress** where a rule allows it, for QUIC and NTP.

## License

Apache-2.0
