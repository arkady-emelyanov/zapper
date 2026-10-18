[← README](../README.md) · [Docs](README.md) · **Commands**

# Commands

## make

| Target | Purpose |
|---|---|
| `make deps` | Download Firecracker, verifying its checksum, and the guest kernel into `~/.local/share/zapper` |
| `make build` | Release build of the static musl `zapper-agent` and of `zapper`, both in `target/release` |
| `make install` | Install both into `~/.local/bin` (set `PREFIX` for another place) |
| `make image [IMAGE=name]` | Build an image from a recipe in the Makefile: `office` (office workers: Claude Code at a pinned version, git, kubectl, podman), `k3s` (a firm's cluster), `ubuntu-24.04` (default; the base profiles), and `claude` and `python` for the config's other profiles |
| `make kernel [KERNEL=k8s]` | Build a guest kernel from the Firecracker config plus `config/kernel/<name>.config`, in a container; `k8s` is the cluster's (~10 min) |
| `make check` | `zapper check` |
| `make test` | Unit tests |
| `make test-e2e` | End-to-end tests with real VMs ([testing](testing.md)): zapper's controls, then the office's |
| `make test-e2e-office` | The office's end-to-end tests only: firms of real VMs with a fake Claude |

## zapper

Global options are `--config FILE` (or `$ZAPPER_CONFIG`; default `config/zapper.yaml` in a checkout's root, else `~/.config/zapper/zapper.yaml`) and `--json`. The CLI reaches the daemon at `$ZAPPER_SOCKET`, or else `$XDG_RUNTIME_DIR/zapper/api.sock`.

Setting up and running the daemon:

| Command | Purpose |
|---|---|
| `zapper init` | Outside a checkout: write the repository's config to `~/.config/zapper/zapper.yaml`, if it doesn't exist yet |
| `zapper check` | Report missing prerequisites, each with the command that fixes it |
| `zapper serve` | Run the daemon; prints the UI URL |

The office ([office](office.md)), as the board:

| Command | Purpose |
|---|---|
| `zapper firm ls` | Firms and their runs' states |
| `zapper firm credentials FIRM [grant WORKER RESOURCE LEVEL \| approve REQUEST \| deny REQUEST \| revoke WORKER RESOURCE] [--level L] [--expires 8h] [--note TEXT]` | Grants and credential requests; or the board grants, decides or revokes (resources: `git`, `cluster`, `drive:<space>`) |
| `zapper firm contracts FIRM` | The run's contracts: hirer, contractor, state, offered, quoted and agreed budgets, how each ended |
| `zapper firm up FIRM [--company NAME] [--ceo NAME] [--seed TOKENS] [--ceo-prompt FILE] [--articles FILE] [--security-share F] [--max-concurrent N] [--daily-turns N] [--scenario NAME]` / `zapper firm down FIRM` | Start a fresh run (facilities, then the CEO, idle; the CEO holds security until they hire an officer), with the board's parameters and charter terms for this run, and optionally a [scenario](office.md#scenarios) / stop it; the run stays on disk |
| `zapper firm scenarios FIRM` | The firm's scenarios: hire slots with extra instructions, and the board's scripted actions |
| `zapper firm pause FIRM` / `zapper firm resume FIRM` | Stop starting turns / start again |
| `zapper firm status FIRM` | Workers' states, what they are doing, mail and desks, turns and tokens today |
| `zapper firm journal FIRM [--run RUN] [-f]` | The run's journal: mail, states, turns, access, publishes |
| `zapper firm transcript FIRM WORKER [--run RUN] [-f]` | A worker's transcript (its agent's output: Claude Code or pi) |
| `zapper firm runs FIRM` / `zapper firm note FIRM RUN TEXT` / `zapper firm prune FIRM RUN` | Past runs, with the board's notes / put a note on one (any text, to find it again; empty removes it; also the Note button in the UI's Runs tab) / delete one |
| `zapper firm replay FIRM RUN` | The UI address that replays a run |
| `zapper firm security FIRM` | Access requests and standing approvals |
| `zapper firm decide FIRM REQUEST --allow\|--deny [--scope once\|worker\|role] [--reason TEXT]` / `zapper firm revoke FIRM RULE` | Decide a request as the board / remove a standing approval |
| `zapper firm screen FIRM CHECK --clear\|--object [--note TEXT]` | Clear a new hire's background check as the board, or object |
| `zapper record status` / `zapper record verify FIRM RUN` | Who keeps the record and whether it's being written / a run's record checked against the keeper's chains and signatures |
| `zapper firm fund FIRM WORKER TOKENS --reason TEXT` | The board gives a worker tokens on top of the approved budget (which grows by that much): for an officer who ran out, say |
| `zapper firm release FIRM WORKER --reason TEXT` | The board lets a worker go; the only way to let a security officer go |
| `zapper firm budget FIRM [approve\|decline REQUEST [--note TEXT]]` | The run's budget, balances and the CEO's budget requests; or the board's answer to one |
| `zapper mail send FIRM --to NAME[,NAME] [--cc …] [--subject S] [--kind K] [--due 2h] [--reply-to ID] [--body TEXT]` | Write as the board; the body is read from stdin if `--body` is missing |
| `zapper mail ls FIRM [--for WORKER] [--unread]` / `zapper mail thread FIRM THREAD` | The board's (or a worker's) inbox / a thread |
| `zapper drive ls FIRM` / `zapper drive get FIRM ARTIFACT[@vN] DIR [--run RUN]` | The shared drive / download an artifact |

In a worker's VM, `office` (the same binary as the MCP server) calls the office tools from a shell: `office desk`, `office mail_send --to dennis --subject … --body … --kind fyi`, `office drive_list`.

Sessions, rules and config: the layer workers run on, for inspecting and debugging. A worker's session is listed in `zapper ps` like any other.

| Command | Purpose |
|---|---|
| `zapper run [-p PROFILE] [-C DIR] [--keep] [-- CMD…]` | Start a session and attach to its command, in a terminal when stdin is one; the session is removed when the command exits, unless `--keep` |
| `zapper attach ID [-- CMD…]` | Another terminal in a running session |
| `zapper exec ID [--system] -- CMD…` | Run a command without a terminal, returning its exit code; `--system` runs it as root without exec supervision, for the operator's own maintenance (e.g. a firm's cluster VM) |
| `zapper cp ID:/path DIR` | Copy a guest directory out |
| `zapper ps` | Sessions with state, image and allow/deny counters |
| `zapper stop ID` / `zapper rm ID` | Stop a session and keep it listed / stop it and remove it from the list; audit logs stay on disk |
| `zapper audit ID` | The session's audit log (JSON lines) |
| `zapper events [-s ID] [--history]` | Follow events, one line each, or raw with `--json` |
| `zapper approvals` | Pending `ask` decisions (outside firms; in a firm, see `zapper firm security`) |
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
zapper firm up acme                          # an idle firm
zapper mail send acme --to barbara --kind assignment --subject "First product" < brief.md
zapper firm journal acme -f                  # watch it work
zapper firm decide acme r-3 --allow --scope worker --reason "package index for the build"
zapper events --history -s tusn49            # what one worker's session did so far
zapper eval -p office-engineer net git.acme.internal:443   # the decision, its rule and layer
zapper run -C $(mktemp -d)                   # a plain shell session, removed on exit
```
