[← README](../README.md) · [Docs](README.md) · **Security model**

# Security model

## Overview

zapper assumes that whatever runs in a session may be hostile, up to a person with a root shell in the guest. The agent inside the guest is treated the same way. Controls that have to hold are enforced on the host. Controls enforced inside the guest are listed separately, as guardrails.

## Threat model

| | |
|---|---|
| Adversary | The session's workload: an agent following injected instructions, or a person trying to break out, possibly with root in the guest. |
| Assets | The host, where the daemon starts VMs and reads secrets. Other sessions. Networks the host can reach, such as management networks, cloud metadata and the host's own services. The values of `proxy` secrets. |
| Goal | The session can do what its policy grants for as long as it lives. Its actions are recorded on the host. |
| Out of scope | A malicious operator. Compromise of the host by other means. KVM and Firecracker vulnerabilities, which are handled by keeping them up to date. What a credential grants and for how long: `guest` secrets are given to the workload by design, so the issuer scopes them and sets their expiry. |

## What is enforced where

| Control | Enforced by | Holds with root in the guest |
|---|---|---|
| Guest traffic goes through zapper. With a NIC, the VMM runs in a network namespace whose only interfaces are loopback and the guest's tap, and nftables there sends TCP to the daemon and DNS to its resolver and drops the rest. In vsock mode the guest has loopback only ([network](network.md)) | Network namespace and nftables outside the VM; Firecracker's device model | Yes |
| DNS answers names that some rule could allow, with per-session synthetic addresses, without contacting any DNS server | Host resolver | Yes |
| Policy for each connection and, when inspected, each request | Host proxy | Yes |
| Address checks: loopback, link-local, metadata and host addresses are unreachable, private ranges only if listed. The host resolves names and connects to the address it checked | Host proxy | Yes |
| A TLS client's server name must equal the name the connection was decided on | Host proxy | Yes |
| `proxy` secrets reach the guest as placeholders. Their values go into allowed requests to the secret's `hosts`, over TLS, or over plain HTTP if the secret sets `plaintext: true` | Host proxy | Yes |
| Each session has its own CA, kept in memory and trusted by that session's guest | Host | Yes |
| Host directories are copied into per-session disks, so host files stay unchanged | Host | Yes |
| Lifetime (`ttl_secs`), CPU, memory and disk size | Host, Firecracker | Yes |
| Limits on input from the guest: line and frame sizes, connections per session, concurrent exec checks, pending approvals | Host | Yes |
| The host accepts one control connection, which the agent makes before the workload starts, then removes the socket | Host | Yes |
| Audit log, terminal recordings, console log (capped at 1 MiB) | Host | Yes |
| Each `execve` and `execveat` of the workload waits for the host's decision | Guest agent (seccomp user notification) | No |
| The 32-bit and x32 syscall tables are blocked, so exec checks can't be bypassed through them | Guest agent (seccomp filter) | No |
| The workload runs as uid 1000 with `no_new_privs`, so setuid binaries keep its privileges | Guest agent | No |
| Terminal connections are accepted from the host's CID only | Guest agent | No |
| `fs` rules (`hidden`, `ro`) | Guest agent (bind mounts at boot) | No |
| Process exit accounting | Guest kernel (`acct`) | No |

The guest-side controls hold while the workload is unprivileged. A profile with `agent.root: true` gives them up, and the UI marks its sessions. A guest kernel exploit also defeats them. The host-side controls hold in both cases.

## Secrets given to a session can be used by it

Every secret a session receives, in any mode, gives the session the authority that secret carries, for as long as the session runs. zapper can't prevent that, so treat each secret you give a session as obtainable by it.

| Mode | The session can read the value | The session can use the authority |
|---|---|---|
| `guest` (env or file) | Yes | Anywhere the network rules let it connect, during the session and afterwards, if it sends the value out |
| `proxy` | No, unless a destination reflects it back | Through requests that zapper attaches it to: allowed by a rule and sent to one of the secret's `hosts`, during the session only |

`proxy` mode protects the value, so the session can't take the raw token elsewhere or keep it after it ends. It doesn't limit what the token can do at those hosts. Choose the token's own scope (repository, permissions, lifetime) as if the session held it.

## Usage

- Run people and agents you don't trust with `root: false`, the default.
- Use `proxy` secrets for bearer tokens and API keys: the workload can use them but can't read them. Use `guest` secrets for credentials the client has to hold, such as S3 signing keys, client certificates and kubeconfigs, and issue them scoped and short-lived.
- Bind each `proxy` secret to the hosts that need it. A destination that echoes request headers back, such as an echo service, returns the value to the guest; `tests/e2e.sh` shows this with a test secret.
- Use a `final: true` deny in the global layer for destinations that must stay unreachable from every profile.
- Leave `daemon.private_networks` empty unless a session needs a private network, and then list that network only.
- Use inspection rules (`methods`, `path`) when a host-level rule is too broad. A passed-through TLS connection to an allowed CDN host can still carry requests for another site on that CDN in its `Host` header.

## Verification

```
make test-e2e      # ends with "all e2e checks passed"
```

`tests/e2e.sh` checks each host-side control. As root in the guest, it tries to get around the NIC capture by using another DNS server, connecting to a real address, and connecting to other ports on the gateway. With `tests/attack.c` as the workload, it tries to:

- impersonate the agent on the control port
- open a root shell over vsock loopback and through the guest's own CID
- exec through the 32-bit and x32 syscall tables
- exec a denied binary

## Limitations

- **Exec checks may race.** The host decides on the path the process passed. A multithreaded process written for it may change that memory between the check and the kernel's own read; the kernel documents this limit of seccomp user notification. The tests haven't shown it, but treat exec policy as a guardrail and rely on network and secrets for the boundary.
- **`fs` rules apply at boot.** Files created later aren't covered, and root in the guest can undo the rules.
- **Firecracker runs as the daemon's user.** It applies its own seccomp filters, and with a NIC it runs in its own user and network namespace, but it can read the files the daemon user can, including secret files. Restricting its view to the session directory, with a mount namespace or the jailer, is planned.
- **The daemon is one process that holds the secrets and runs the proxy.** It parses HTTP, TLS, JSON and frames from the guest with memory-safe libraries, and a bug in that code would be a bug in the boundary.
- **Rate and volume limits** for network traffic are the next item: see [the README](../README.md#next).
- **Sessions end when the daemon restarts.** Their audit logs stay on disk.

Network-specific limitations are on the [network](network.md#limitations) page.

## References

- Firecracker design and threat model: https://github.com/firecracker-microvm/firecracker/blob/main/docs/design.md
- seccomp user notification and its TOCTOU caveat: `man 2 seccomp_unotify`
- Sources: `crates/zapper/src/netns.rs`, `crates/zapper/src/daemon/{proxy,dns,nic,addr,session,tls}.rs`, `crates/zapper-agent/src/{shell,sys,init}.rs`
