[← README](../README.md) · [Docs](README.md) · **Testing**

# Testing

## Tiers

| Tier | Command | Needs | Time | Checks |
|---|---|---|---|---|
| Unit | `make test` | Rust toolchain | seconds | Rule evaluation and layering, final rules, defaults, inspection, DNS answers, config validation and round-trip, rule ids, templates, frame codec, address checks, SNI parsing, certificate minting, sizes |
| End to end | `make test-e2e` | KVM, network access, user namespaces, the `ubuntu-24.04` image, gcc | ~40 s | Below |

`tests/e2e.sh` starts its own daemon with a temporary config, socket and state directory, separate from any running daemon. It removes the sessions it starts, and deletes its directory when all checks pass, or keeps it when `ZAPPER_E2E_KEEP=1` is set. It ends with `all e2e checks passed`, or with the number of failures and the path of the daemon log.

## What the end-to-end tier checks

| Group | Checks |
|---|---|
| NIC session | Runs as uid 1000 with `eth0` and without proxy variables. The workdir is copied in, and `fs: hidden` hides `.env`. A `proxy` secret appears as its placeholder and a `guest` secret as its value. DNS answers an allowed name with a synthetic address and refuses others. An allowed TLS connection succeeds, and an allowed request gets its secret. POST to a GET-only host, a port without a rule, a raw IP, and a TLS name that differs from the dialled name are denied. An exec rule denies `perl`, and a denied first command prints the reason and exits 126. The exit code reaches the CLI. |
| Root in the guest | With another DNS server configured, names are still answered by policy. A connection to a real address is denied, and other ports on the gateway are closed. |
| vsock session | Loopback only, with proxy variables. An allowed connection succeeds through the proxy, and a direct connection fails. |
| Approvals | A request waits for approval, goes through once approved, and `--remember session` adds a rule. |
| Config | A rule change shows in the diff, a global final rule wins, an invalid change is rejected, and reload discards changes. |
| Attacks (`tests/attack.c` as the workload) | Impersonating the agent on the control port, opening a shell over vsock loopback or the guest's own CID, and exec through the 32-bit or x32 tables all fail; the last two are killed with SIGSYS. A denied exec fails. |

A new control needs a check here, including one that attacks it from inside the guest.

## Manual scenarios

Before a release, walk through these:

| Scenario | Expected |
|---|---|
| `zapper run`, resize the terminal, run `vim`, press Ctrl-C during a command | Full-screen programs redraw, and Ctrl-C stops the command in the guest |
| `zapper run -p claude` with a token in place, and ask for a change to a file | Claude makes the change; `zapper events` shows `api.anthropic.com` with `secrets: [claude]` |
| A raw TCP connection to `github.com:22` from a session that allows it | The SSH banner arrives within a second |
| Kill the daemon with SIGKILL during a session | Its `firecracker` processes exit, and the next `zapper serve` removes the leftover disks |
| SIGTERM to the daemon | Each session stops with `daemon shutdown` |
| A profile with `ttl_secs: 60` | The session stops after a minute with `ttl expired` |
| Open the UI URL, then the same URL with another `Host` header | The UI loads, and the other host gets 403 |

## References

- Sources: `tests/e2e.sh`, `tests/attack.c`, and the `#[cfg(test)]` modules in each crate
