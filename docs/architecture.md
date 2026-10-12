[← README](../README.md) · [Docs](README.md) · **Architecture**

# Architecture

## Overview

Terms used on this page:

- **Host**: your machine, where zapper runs.
- **Session**: one sandbox. Each session is one small virtual machine.
- **Guest**: the inside of that virtual machine.
- **Workload**: the program you run in the guest, such as Claude Code or a shell.

```
            HOST                                        GUEST (one per session)
 ┌──────────────────────────────────┐            ┌──────────────────────────────┐
 │ zapper CLI ──▶ zapper daemon      │            │ zapper-agent                 │
 │                 │                 │   vsock    │  sets up the guest           │
 │                 ├── starts VM ────┼───────────▶│  runs your program           │
 │                 ├── decides ◀─────┼── "may I?" │  asks before each program    │
 │                 └── proxy ◀───────┼── network ─│  starts                      │
 │                        │          │            │                              │
 └────────────────────────┼─────────┘            │ workload (uid 1000)          │
                          ▼                       └──────────────────────────────┘
                   internet, lab
```

zapper has four parts:

| Part | Runs | Job |
|---|---|---|
| `zapper` CLI | on the host, when you type a command | Talks to the daemon |
| `zapper` daemon | on the host, started by `zapper serve` | Starts sessions, decides what they may do, records what they did |
| `zapper __vmm` | on the host, one per session | Starts Firecracker locked in its own namespaces |
| `zapper-agent` | in the guest, as its first process | Sets the guest up and runs the workload |

All decisions are made in the daemon. The agent in the guest asks; it doesn't decide.

## How it works

### Starting a session

What happens after `zapper run`:

1. The CLI asks the daemon for a new session with a profile.
2. The daemon gives the session a directory, `state/sessions/<id>/`.
3. It gives the session its own copy of the image, which becomes the guest's system disk. Folders you mount, like your project, are copied into disks of their own: the guest works on that copy, and `zapper cp` brings results back.
4. It starts Firecracker through `zapper __vmm`, which hides the rest of the host from it ([security](security.md#the-vmm)).
5. The guest boots. Its first process is `zapper-agent`.
6. The agent connects to the daemon and receives the setup: user, environment, files, disks.
7. The agent sets up the guest and reports ready. This takes about a second.
8. The CLI opens a terminal to the workload.

When the workload exits, the daemon stops the VM and deletes its disks. The audit log and the terminal recordings stay.

### The session directory

```
state/sessions/<id>/
  audit.jsonl        every event of the session
  shell-1.cast       terminal recording
  console.log        boot messages
  vm/                the only part Firecracker can see; deleted at the end
    root.ext4          the guest's disk, a copy of the image
    disk0.ext4         a copy of a mounted host directory
    vm.json            Firecracker's configuration
    v.sock, nic.sock   sockets between guest and daemon
```

### How the guest talks to the host

The guest and the daemon use vsock, a direct channel between a VM and its host. It doesn't go through any network.

| Channel | Opened by | Used for |
|---|---|---|
| Control | the agent, once at boot | Setup, "may I run this program?" questions, events, shutdown |
| Terminal | the daemon, once per terminal | Your keystrokes in, the workload's output out |
| Network | the guest, per connection | The guest's internet traffic ([network](network.md)) |

The daemon accepts one control connection per session. After that the socket is removed, so a workload can't pretend to be the agent.

The CLI talks to the daemon over a unix socket that only your user can open. The web UI, for watching and answering approvals, uses `127.0.0.1:7878`. Both are described on the [API](api.md) page.

### Checking each program

Before the workload starts, the agent installs a filter in it that the workload can't remove. Every child process inherits it. Each time the workload starts a program:

1. The kernel pauses the workload.
2. The agent reads which program it wants to run, and sends that to the daemon.
3. The daemon checks the `exec` rules. With `ask`, it waits for your answer.
4. The agent lets the program start, or refuses it.

If the very first command is refused, the agent prints why and exits with code 126.

### The guest's agent

`zapper-agent` doesn't live in the image. The daemon packs it into a small boot file (an initramfs) each time it starts. So any Linux image works without changes, and the agent always matches the daemon's version.

## Configuration

See [configuration](configuration.md) and [policy](policy.md).

## Verification

`make test` checks the rules and the protocols without starting VMs. `make test-e2e` starts real sessions and checks each part ([testing](testing.md)).

## Limitations

- Firecracker can't share folders with the host. Mounts are copies, and `zapper cp` copies results back out.
- More: [security](security.md#limitations), [network](network.md#limitations).

## References

- Firecracker vsock: https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md
- Sources: `crates/zapper/src/daemon/`, `crates/zapper/src/vmm.rs`, `crates/zapper/src/netns.rs`, `crates/zapper-agent/src/`, `crates/zapper-policy/src/proto.rs`
