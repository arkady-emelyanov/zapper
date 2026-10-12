# Instructions for coding agents

People should start with the [README](README.md). Operating a running daemon (starting sessions, changing rules, answering approvals, reading audit logs) is covered by the agent skill [skills/zapper/SKILL.md](skills/zapper/SKILL.md); keep it in step with the CLI and docs.

zapper runs programs (a coding agent, an interview candidate's shell, a language sandbox) in Firecracker microVMs. All guest traffic is captured outside the VM and goes through policy on the host. Credentials are either injected by the host or handed to the guest, per secret. Every exec is checked by the host, and everything is recorded. Rules are layered (session → profile → global) and managed through a REST API with a running/startup config like a router's.

## Setting zapper up for a user

### 1. Check the host (no changes)

```
make deps && make build && target/release/zapper check     # every line "ok", or the fix
```

`check` reports `/dev/kvm` access, Firecracker, the guest kernel, the agent binary, images per profile, unprivileged user and mount namespaces (for VMM confinement), `ip` and `nft` (for NICs), podman and e2fsprogs, config permissions and whether the daemon is up. Report what is missing and let the user decide.

### 2. Ask for root once, with reasons

zapper itself never needs root. Only host prerequisites may:

| Command | Why it needs root |
|---|---|
| `sudo usermod -aG kvm $USER` | `/dev/kvm` access, if `check` reports none (then log in again) |
| `sudo apt install podman e2fsprogs` | building images, if missing |
| `sudo apt install iproute2 nftables` | guest NICs, if missing |
| `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0` | VMM confinement, if user namespaces are restricted |

Never run `sudo` without the user's explicit consent, and run only what you listed.

### 3. Build

```
make deps                       # Firecracker (checksum-verified) and kernel, ~1 min
make install                    # zapper and zapper-agent into ~/.local/bin
zapper init                     # ~/.config/zapper/zapper.yaml from config/zapper.example.yaml
make image                      # ubuntu-24.04, ~2 min; make image IMAGE=claude for Claude Code
```

Run image builds in the background with a log (`make image > /tmp/zapper-image.log 2>&1`) and give the user `tail -f /tmp/zapper-image.log`.

### 4. Hand over

Start `zapper serve` (or tell the user to), and give them the UI URL it prints and `zapper run` to try. For Claude Code, the user creates the token secret themselves (`claude setup-token`, stored 0600 in the file the config names); never read or print it.

### Rules

- Sessions may be in use: before `zapper rm`, `zapper stop`, `zapper config reload` or restarting the daemon, check `zapper ps` and `zapper config diff` and say what you are about to change. Unsaved running config is lost on restart.
- `zapper config save` writes the user's startup file: run it when the user asks to keep a change.
- Do not print `zapper config startup` output into chats when it may hold inline secrets; values are redacted, but names and sources are not.
- The daemon's `state` directory holds audit logs and terminal recordings of sessions: treat them as sensitive and do not delete them unasked.

## Working on the repository

Architecture: [docs/architecture.md](docs/architecture.md). Network: [docs/network.md](docs/network.md). Security model and the controls that must hold: [docs/security.md](docs/security.md).

```
crates/zapper-policy/    config model, layered rule evaluation, host↔guest protocol (no I/O)
crates/zapper/           CLI, daemon, proxy, Firecracker launcher, REST API, web UI
  src/vmm.rs               `zapper __vmm`: runs Firecracker confined in its own namespaces and root
  src/netns.rs             the guest's network in the VMM's namespace, and traffic capture
  src/daemon/session.rs    session lifecycle, guest control channel, exec decisions, recordings
  src/daemon/proxy.rs      egress proxy: tunnels, TLS inspection, secret injection
  src/daemon/addr.rs       which destination addresses are reachable at all
  src/daemon/dns.rs        the guest's resolver: synthetic addresses for allowed names
  src/daemon/vm.rs         disks, initramfs, the Firecracker process
  src/daemon/ui.html       the web UI: state, terminal viewer, answering approvals (textContent only)
  src/daemon/assets/       xterm.js 6.0.0 (MIT) for the terminal viewer; see its README
crates/zapper-agent/     guest PID 1: setup, terminals, seccomp exec supervision, accounting
config/                  zapper.example.yaml, copied by `zapper init`
scripts/fetch-deps.sh    Firecracker and kernel download
tests/e2e.sh             end-to-end tests against real VMs; tests/attack.c probes from inside
docs/                    one page per topic; index in docs/README.md
skills/zapper/           operating skill, linked from .claude/skills/zapper
```

Build and test:

```
make build        # release: static musl agent + host binary
make test         # unit tests, no VMs, seconds
make test-e2e     # real VMs, network; ~40 s; must end "all e2e checks passed"
```

Invariants (each has a test in `tests/e2e.sh` or a unit test; a change that breaks one is wrong even if it compiles):

- The VMM runs in its own user, network and mount namespaces; its root holds the session's `vm/` directory, the kernel, initramfs, its binary and its devices.
- Guest traffic goes through zapper, also with root in the guest. With a NIC, the VMM's network namespace has loopback and the tap as its interfaces and sends all TCP and DNS to the daemon; in vsock mode, vsock is the guest's only channel to the host.
- DNS answers names that some rule could allow, and answers without contacting a DNS server.
- `proxy` secret values stay on the host. The proxy adds a value to a request when a rule listing the secret allows it, the destination matches the secret's `hosts`, and the connection uses TLS or the secret sets `plaintext: true`. A `guest` secret goes to sessions whose profile uses it.
- The proxy connects to the address it checked. Loopback, link-local and host addresses are unreachable, and private ranges need `daemon.private_networks`.
- Passthrough connections are bound to the TLS server name; inspected ones present a per-session CA and override `Host`.
- The host accepts only the first control connection; the agent accepts terminal connections only from CID 2.
- The seccomp filter kills foreign-arch and x32 syscalls, so exec checks cannot be bypassed through another syscall table.
- Anything from the guest is bounded (line, frame, connection and approval limits) and rendered as text in the UI.
- The UI's only write is answering approvals, and it requires the token cookie, a local `Host`, `X-Zapper-UI: 1` and a local `Origin`.
- A config change is validated as a whole and applied atomically, or not at all.

Applying changes: rebuild with `make build` and restart the daemon; sessions do not survive a restart (stop them first, see Rules). Rule and profile changes need no restart: they apply to running sessions at once.

Markdown: never hard-wrap prose; one line per paragraph or list item.
