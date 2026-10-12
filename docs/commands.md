[← README](../README.md) · [Docs](README.md) · **Commands**

# Commands

## make

| Target | Purpose |
|---|---|
| `make deps` | Download Firecracker, verifying its checksum, and the guest kernel into `~/.local/share/zapper` |
| `make build` | Release build of the static musl `zapper-agent` and of `zapper`, both in `target/release` |
| `make install` | Install both into `~/.local/bin` (set `PREFIX` for another place) |
| `make image [IMAGE=name]` | Build an image from a recipe in the Makefile: `ubuntu-24.04` (default), `claude`, `python` |
| `make check` | `zapper check` |
| `make test` | Unit tests |
| `make test-e2e` | End-to-end tests with real VMs ([testing](testing.md)) |

## zapper

Global options are `--config FILE` (or `$ZAPPER_CONFIG`; default `~/.config/zapper/zapper.yaml`) and `--json`. The CLI reaches the daemon at `$ZAPPER_SOCKET`, or else `$XDG_RUNTIME_DIR/zapper/api.sock`.

| Command | Purpose |
|---|---|
| `zapper init` | Write the example config, if the file doesn't exist yet |
| `zapper check` | Report missing prerequisites, each with the command that fixes it |
| `zapper serve` | Run the daemon; prints the UI URL |
| `zapper run [-p PROFILE] [-C DIR] [--keep] [-- CMD…]` | Start a session and attach to its command, in a terminal when stdin is one; the session is removed when the command exits, unless `--keep` |
| `zapper attach ID [-- CMD…]` | Another terminal in a running session |
| `zapper exec ID -- CMD…` | Run a command without a terminal, returning its exit code |
| `zapper cp ID:/path DIR` | Copy a guest directory out |
| `zapper ps` | Sessions with state, image and allow/deny counters |
| `zapper stop ID` / `zapper rm ID` | Stop a session and keep it listed / stop it and remove it from the list; audit logs stay on disk |
| `zapper audit ID` | The session's audit log (JSON lines) |
| `zapper events [-s ID] [--history]` | Follow events, one line each, or raw with `--json` |
| `zapper approvals` | Pending `ask` decisions |
| `zapper approve ID [--remember session]` / `zapper deny …` | Answer one, for this request or the rest of the session |
| `zapper rules [--scope S]` | List rules of all layers, or of `global`, `profile:NAME` or `session:ID` |
| `zapper rule add SCOPE 'YAML' [--at N]` / `zapper rule rm ID` | Change rules in the running config |
| `zapper eval [-s ID\|-p PROFILE] net HOST[:PORT] [METHOD PATH]` | Dry run of a connection or request |
| `zapper eval [-s ID\|-p PROFILE] exec PATH [ARGV…]` | Dry run of an exec |
| `zapper config show\|startup\|diff\|save\|reload\|edit\|apply FILE` | [Running and startup config](configuration.md#running-and-startup-config) |
| `zapper profile ls\|show NAME\|set NAME FILE\|rm NAME` | Profiles in the running config |
| `zapper image build NAME --from REF [--run CMD]… [--size S]` / `zapper image ls` | [Images](images.md) |

Examples:

```
zapper run                                   # a shell on a copy of $PWD, removed on exit
zapper run -p claude -- claude -p "fix the failing test"
zapper run --keep -p slurm-interview         # kept afterwards; `zapper attach ID` opens it again
zapper events --history -s tusn49            # what that session did so far
zapper eval -p claude net github.com:443 POST /x/y/pulls   # the decision, its rule and layer
```
