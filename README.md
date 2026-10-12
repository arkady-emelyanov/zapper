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

- Each session is a small Linux virtual machine (Firecracker).
- Your program runs inside it as a normal user.
- The VM gets a copy of your project folder, not the folder itself.
- All its network traffic goes through zapper on the host.
- zapper checks each connection and each started program against your rules.
- Rules say allow, deny, or ask you first.
- Tokens can be added by the host on the way out, so the program never sees them.
- Everything the session does is logged, and terminals are recorded.
- You manage it with the `zapper` command. A local web page shows what's happening and lets you answer approvals.
- Rule changes apply at once; `zapper config save` keeps them.

More: [architecture](docs/architecture.md), [network](docs/network.md), [policy](docs/policy.md), [security](docs/security.md).

## Quickstart

On a Linux host with KVM:

```
make deps                     # Firecracker and a guest kernel into ~/.local/share/zapper (~1 min)
make install                  # zapper and zapper-agent into ~/.local/bin
zapper init                   # ~/.config/zapper/zapper.yaml from the example
make image                    # base Ubuntu image (~2 min); `make image IMAGE=claude` for Claude Code
zapper check                  # each line "ok", or the command that fixes it
zapper serve                  # the daemon; prints the UI URL
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

The [quickstart](docs/quickstart.md) walks through this step by step. [docs/commands.md](docs/commands.md) lists every command, and [docs/README.md](docs/README.md) indexes the documentation.

## Status

zapper is early software that works end to end. `make test-e2e` boots real VMs and checks each control from inside the guest, including attempts to get around the controls as root. [docs/security.md](docs/security.md) lists the known limitations.

## Next

1. **Network rate and volume limits** per session: bandwidth and packet rate on the NIC, using Firecracker's rate limiter, and byte counts per destination and per session in the proxy, with an event when a limit is hit.
2. **UDP egress** where a rule allows it, for QUIC and NTP.

## License

Apache-2.0
