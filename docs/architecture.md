[← README](../README.md) · [Docs](README.md) · **Architecture**

# Architecture

## Overview

zapper is built from three crates:

| Crate | Binary | Role |
|---|---|---|
| `zapper-policy` | (library) | Config model, layered rule evaluation, and the host↔guest protocol. It does no I/O and is unit-tested. |
| `zapper` | `zapper` | The CLI and the daemon: sessions, Firecracker, the network namespace shim, the egress proxy, the REST API and the read-only UI. |
| `zapper-agent` | `zapper-agent` | A static musl binary that runs as PID 1 in every guest. |

## How it works

### A session's life

1. `zapper run` sends `{profile, workdir}` to `POST /v1/sessions`.
2. The daemon resolves the profile's `extends` chain. Engine, agent and mounts are merged into one `ResolvedProfile`, while the policy layers stay separate.
3. It prepares the session directory `state/sessions/<id>/`:
   - `root.ext4`, a reflink or sparse copy of the image, grown to `engine.disk`
   - `diskN.ext4` for each mount, a copy of the host directory made with `mkfs.ext4 -d`
   - a new CA, held in memory
4. It listens on `v.sock_1000` (control), `v.sock_3128` (proxy) and, for a NIC, `nic.sock`. Then it starts Firecracker with:
   - the kernel
   - an initramfs holding `zapper-agent` as `/init`, built from the agent binary each time the daemon starts, so images stay plain and the agent matches the daemon
   - the disks and a vsock device
   - for `network: nic`, a virtio NIC attached to `tap0`, with Firecracker running in the session's network namespace ([network](network.md))
5. The agent mounts `/dev/vda`, switches root into it, mounts `/proc`, `/dev` and the other system filesystems, brings up loopback, and connects to the host on vsock port 1000. With a NIC, the kernel has already configured `eth0` from its command line.
6. The host sends `Init` with the `GuestSpec`: the user, the environment (placeholders for `proxy` secrets, values for `guest` secrets), files such as the rendered agent configuration, disks, `fs` rules, setup commands and the CA. Then it removes the control socket, so later connections to that port fail.
7. The agent:
   - creates the uid 1000 user
   - mounts the disks and writes the files
   - installs the CA
   - applies the `fs` rules
   - turns on process accounting
   - sets up DNS (NIC) or starts the proxy forwarder (vsock)
   - runs the setup commands
   - sends `Ready`
8. The daemon emits `session running`, about 0.4 s after the request in vsock mode and 0.9 s with a NIC. The CLI then opens a terminal: `GET /v1/sessions/<id>/attach` upgrades the connection, and the daemon connects to the guest's vsock port 1024 and relays frames, recording them.
9. When the command exits, the TTL runs out or `zapper rm` is called, the host sends `Shutdown`, waits up to 5 s, stops the VMM and deletes the disks. The audit log, console log and recordings are kept.

### Channels

| Channel | Direction | Carries |
|---|---|---|
| vsock 1000 | guest → host, once | JSON lines: `Init`, `Ready`/`Failed`, `ExecCheck`/`ExecReply`, events, `Shutdown` |
| vsock 1024 | host → guest, per terminal | a `ShellRequest` line, then frames: data, stderr, resize, eof, exit |
| vsock 3128 | guest → host, per connection | HTTP proxy protocol, in vsock mode |
| `nic.sock` in the session directory | namespace shim → daemon, per connection or query | `TCP <ip> <port>` then the stream, or `DNS <len>` then the query and a length-prefixed answer |
| `$XDG_RUNTIME_DIR/zapper/api.sock` (0600) | CLI → daemon | REST API and terminal upgrades |
| TCP `127.0.0.1:7878` | browser → daemon | the read-only UI and its API |

Each VM has its own sockets, so the daemon knows a connection's session from the socket it arrives on.

### Network

Covered on its own page: [network](network.md).

### Exec supervision

For each terminal, the agent forks the workload. The child process:

1. drops to uid 1000, unless the profile sets `root`
2. sets `no_new_privs`
3. installs a seccomp filter that sends `execve` and `execveat` to the agent, kills foreign-architecture and x32 calls, and allows the rest
4. hands the filter's listener fd to the agent
5. execs the command

Every descendant inherits the filter, and it can't be removed. For each notification, the agent reads the path and argv from `/proc/<pid>/mem`, resolves the path against the process's cwd or dirfd, and sends `ExecCheck`. The process waits while the host evaluates the `exec` rules, which may include waiting for an approval. The agent then lets the call continue or fails it with `EACCES`. If the session's first command is denied, the agent prints the reason and the command exits with 126.

## Configuration

See [configuration](configuration.md) and [policy](policy.md).

## Verification

`make test` covers rule evaluation, layering, protocol framing, address checks, DNS answers, SNI parsing and certificate minting. `make test-e2e` runs the whole path with real VMs ([testing](testing.md)).

## Limitations

See [security model: limitations](security.md#limitations) and [network: limitations](network.md#limitations). Firecracker can't share a filesystem with the host, so mounts are copies; `zapper cp` copies results out.

## References

- Firecracker vsock: https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md
- Sources: `crates/zapper/src/daemon/`, `crates/zapper/src/netns.rs`, `crates/zapper-agent/src/`, `crates/zapper-policy/src/proto.rs`
