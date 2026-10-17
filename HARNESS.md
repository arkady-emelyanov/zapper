# Agent harnesses

A harness is the coding agent a session runs: Claude Code today, and pi or opencode later. zapper sees an agent only through the interfaces in `crates/zapper/src/daemon/harness.rs`, so adding one is an implementation of those interfaces plus a name in `agent.kind` (`zapper_policy::AGENT_KINDS`). This page describes the interfaces, how Claude Code implements them, and how pi and opencode would. Neither is implemented yet. The pi and opencode details come from their repositories and documentation as of 2026-10-17 (pi `@earendil-works/pi-coding-agent` 1.3, opencode `@opencode/cli` 2.0); check them again before implementing.

## The interfaces

**Configuration, in neutral terms.** A profile's `agent` section says what the agent should have; the harness says where and how its agent reads it ([configuration](docs/configuration.md#agent)):

| Key | Meaning |
|---|---|
| `instructions` | Standing instructions, written to the harness's instructions file |
| `skills` | A host directory of `<name>/SKILL.md` skills, mounted read-only where the harness reads skills |
| `mcp_servers` | MCP servers as `{name: {command, args, env}}`, written in the harness's format |
| `turn_end` | A command to run when the agent is about to end a turn; if it prints a reason, the agent goes on with it |
| `model`, `permission_mode` | The model, and how the agent approves its own actions |
| `settings` | Harness-native settings merged into its main config file |
| `config_files`, `config_mounts` | Files and host directories placed relative to the harness's config directory |

The office uses only these to set a worker up: the preamble as `instructions`, the role's skills as `skills`, the office tools (`zapper-agent office mcp`) in `mcp_servers`, and `zapper-agent office stop-hook` as `turn_end`.

**`Harness`**: setting the agent up in the guest.

- `kind()`: its name in `agent.kind`.
- `config_dir(home)`: where it reads its configuration; `config_files` and `config_mounts` are relative to it.
- `skills_dir(home)`: where it reads skills; by default `<config_dir>/skills`.
- `guest(profile, home, env, files)`: files and environment from the neutral keys, such as settings, the instructions file, the MCP config, the turn-end hook, and switching off update checks.
- `mounts(agent, home)`: the skills directory and `config_mounts`; the default implementation is usually enough.
- `driver()`: how to run it as an office worker, if it can be one.

**`Driver` and `Conversation`**: running the agent headless, one long conversation per worker. The foreman opens a terminal channel (no tty) to the command, writes lines to its stdin, and reads JSON lines from its stdout. A `Conversation` is one agent process. It is stateful, so protocols with request ids and sessions keep them there.

- `argv()`: the command line, continuing an earlier conversation when given a session id.
- `opening()`: lines to send first, such as a protocol handshake.
- `prompt(text)`: the line that gives the agent a prompt (a wake).
- `read(line)`: what an output line means, as `Event`s:
  - `Session(id)`: the conversation's id, for resuming it after a restart.
  - `Activity(text)`: what the agent is doing now (`Edit pricing.md`), for the UI.
  - `TurnEnd { tokens, outcome, detail }`: the turn ended. `tokens` counts input, output and cache writes against budgets. `outcome` is `Done`, `RateLimited` (the foreman retries the same wake later) or `Failed(why)` (the wake is kept and the worker is marked in error). `detail` goes into the journal's `turn end` record.
  - `Send(line)`: a reply the protocol expects, which the foreman writes back.
- `steers()` and `steer(text)`: hand the agent a message while it works, where the harness supports that natively. Without it, the `turn_end` command hands mail over when the agent is about to stop (`foreman::stop_check`).

The fake Claude (`zapper-agent fake-claude`) is the test double for office e2e tests. It speaks Claude Code's stream-json and runs the `turn_end` hook from the settings the way Claude Code does. A new harness either gets its own fake or is tested through the same events.

## Claude Code (`claude-code`)

| | |
|---|---|
| Config directory | `~/.claude` |
| Settings | `~/.claude/settings.json` (`model`, `settings`, the `Stop` hook); `~/.claude.json` (onboarding and trust accepted, `mcpServers`); `/etc/claude-code/managed-settings.json` (root-owned: `permission_mode`) |
| Instructions | `~/.claude/CLAUDE.md` |
| Skills | `~/.claude/skills` |
| `turn_end` | a `Stop` hook; it blocks the stop with the reason it prints |
| Environment | `DISABLE_AUTOUPDATER=1`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`; the token from the profile (`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`, added by the proxy) |
| Command | `claude -p --input-format stream-json --output-format stream-json --verbose --permission-mode bypassPermissions [--resume ID]` |
| Prompt | `{"type":"user","message":{"role":"user","content":[{"type":"text","text":…}]}}` |
| Output | `session_id` on events; `assistant` events with `tool_use` blocks (activity); `result` ends a turn, with `usage` (`input_tokens`, `output_tokens`, `cache_creation_input_tokens`), `is_error` and `result` (rate limits and API failures are recognised in its text) |
| Mid-turn mail | the `Stop` hook (`turn_end`) |
| Install | the `office` image installs a pinned version |

## pi (`pi`)

pi is the coding agent in [earendil-works/pi](https://github.com/earendil-works/pi), package `packages/coding-agent`, npm `@earendil-works/pi-coding-agent`, binary `pi`, on Node 22.19 or later.

**Install.** Install it in the image at a pinned version: `npm i -g --ignore-scripts @earendil-works/pi-coding-agent@<version>`. pi doesn't update itself; set `PI_SKIP_VERSION_CHECK=1`, and `PI_OFFLINE=1` to stop automatic network use.

**Configuration** (`docs/configuration.md`, `docs/settings.md`):

| Neutral key | pi |
|---|---|
| config directory | `~/.pi/agent`, or anywhere through `PI_CODING_AGENT_DIR` |
| `settings`, `model` | `settings.json`: `defaultProvider`, `defaultModel`, `defaultThinkingLevel`, `defaultTools`, `retry.*`, `sessionDir` |
| `instructions` | `~/.pi/agent/AGENTS.md`. pi also reads `CLAUDE.md`, and `AGENTS.md` from the working directory up, which a worker's empty `/work` doesn't have |
| `skills` | `~/.pi/agent/skills` (also `~/.agents/skills`, `--skill <path>`); it implements the Agent Skills spec |
| `mcp_servers` | `~/.pi/agent/mcp.json`, `{"mcpServers":{name:{command,args,env,cwd}}}`: the neutral shape. MCP is the built-in `builtin:mcp` extension; tools are named `mcp__<server>__<tool>` |
| `permission_mode` | nothing to set. pi has no approval system ("It does not ask before every tool call"); zapper's sandbox (network policy, exec checks) is the only control. Tools can be limited with `--tools` and `--exclude-tools` |
| project trust | a headless run needs `--approve` or `"defaultProjectTrust": "always"` to load `.pi/` project settings; workers don't need them |
| auth | `ANTHROPIC_API_KEY`, `ANTHROPIC_OAUTH_TOKEN` (a `sk-ant-oat…` token, as `claude setup-token` makes), or `ANTHROPIC_AUTH_TOKEN`; or `auth.json`. The base URL is in `models.json` (`providers.anthropic.baseUrl`), not an environment variable; zapper's proxy doesn't need one |

**Driving.** `pi --mode rpc` is a long-running process speaking JSON lines on stdin and stdout (`docs/rpc.md`, `docs/rpc-commands.md`).

- `argv`: `pi --mode rpc --session-id <id>`, which opens the session or creates it. The host can pick a stable id per worker, so resuming needs no id from the output. Sessions live in `~/.pi/agent/sessions/` (`--session-dir`, `PI_CODING_AGENT_SESSION_DIR`).
- `prompt`: a `prompt` command, `{"type":"prompt","id":…,"message":…}`. Responses are `{"type":"response","id",…}`.
- `read`:
  - `tool_execution_start {toolName, args}` → `Activity`.
  - `message_end` for assistant messages carries `usage {input, output, cacheRead, cacheWrite, totalTokens}`, `stopReason` (`stop|length|toolUse|error|aborted`) and `errorMessage`. Sum `input + output + cacheWrite` over the turn's messages.
  - `auto_retry_start {attempt, delayMs, errorMessage}` and `auto_retry_end {success, finalError}`: rate limits and overloads, which pi retries itself. A final failure → `Outcome::Failed` or `RateLimited` by its message.
  - `agent_settled {aborted}` ends the turn → `TurnEnd`. The documentation says `agent_end` alone is not final.
  - `get_state` returns `sessionId` and `sessionFile`, if the id is needed.
- `steer`: pi takes messages while it works. `follow_up` is delivered when the agent would otherwise stop (the same as Claude Code's Stop hook), and `steer` after the current tool calls. `steers()` is true, and the foreman hands mail over as it arrives. `turn_end` then isn't needed; if it were, pi runs no shell hooks, so it would take an extension (`agent_before_settle` can return `continue: true`).

**Open questions.**
- Whether pi's RPC mode stays up across many prompts without growing state the host must clear.
- How it reports a refused token: probably `stopReason: error` with an `errorMessage`.
- Its tool names in activity: `toolName` is enough.

## opencode (`opencode`)

opencode is [anomalyco/opencode](https://github.com/anomalyco/opencode), npm `@opencode/cli` (V2; V1 was `opencode-ai`), binary `opencode`, a native build behind a Node launcher. **V2 changed a lot**: it renamed config keys, replaced the server API, reads only `AGENTS.md`, and has no Claude subscription login. Implement against V2 and its documentation in `services/www/src/docs/content/`.

**Install.** Install it in the image at a pinned version (`npm i -g @opencode/cli@2.0.x`), with `"update": "disable"` in its config and `OPENCODE_DISABLE_AUTOUPDATE=1`.

**Configuration** (`config.mdx`, `policies.mdx`): `opencode.json` or `.jsonc`, in increasing precedence: global `~/.config/opencode/`, the project, `OPENCODE_CONFIG_CONTENT`, then the managed `/etc/opencode/`, which users and projects can't override.

| Neutral key | opencode |
|---|---|
| config directory | `~/.config/opencode` (`OPENCODE_CONFIG_DIR`) |
| `settings`, `model` | `opencode.json`: `model` (`provider/model#variant`), `permissions`, `agents`, `commands`, `instructions[]`, `providers`, `plugins` |
| `instructions` | `~/.config/opencode/AGENTS.md`; extra files go in `instructions[]`. V2 doesn't read `CLAUDE.md` |
| `skills` | `~/.config/opencode/skills` (also `~/.claude/skills`, `~/.agents/skills`, `skills[]`); the model loads them through a `skill` tool |
| `mcp_servers` | `"mcp": {"servers": {name: {"type": "local", "command": [command, ...args], "environment": env}}}`, converted from the neutral shape. V2 exposes MCP tools through "Code Mode" by default; set `codemode: false` to expose them as tools |
| `permission_mode` | the managed `/etc/opencode/opencode.json`, root-owned like Claude Code's managed settings: `{"action":"*","resource":"*","effect":"allow"}`, or `--auto` on the command line. Without either, a headless run rejects every permission request. V2 allows `external_directory` by default |
| auth | `ANTHROPIC_API_KEY` from the environment, or `providers.anthropic.settings.apiKey` / `.authToken` (a bearer token) and `.baseURL`. There's no Claude Pro/Max login; whether a `claude setup-token` token works as `authToken` is unverified. Accounts from `opencode auth login` live in its database and take precedence over the environment |

**Driving.** opencode has no stdin streaming prompt mode like `claude -p`. Two ways fit:

1. **ACP over stdio (preferred).** `opencode acp` speaks the Agent Client Protocol: JSON-RPC 2.0, one message per line on stdin and stdout, which fits `Conversation` as it is.
   - `opening`: `initialize`, then `session/new` (or `session/load` with the id to resume).
   - `prompt`: a `session/prompt` request with the session id from `session/new`'s response, which the conversation keeps.
   - `read`:
     - `session/update` notifications → `Activity` (tool calls) and text.
     - The `session/prompt` response with its `stopReason` → `TurnEnd`.
     - Requests from the agent, such as `session/request_permission` → `Send(reply)`, allowing them as the profile's `permission_mode` says.
   - Token usage: check what opencode's ACP reports per turn. If it doesn't report usage, read it from `opencode` with `session.step.ended` events, or count from the database.
2. **HTTP.** `opencode serve` (or `--standalone`) per worker, with `POST /api/session/:id/prompt` and the `GET /api/event` SSE stream. Its run lifecycle events end turns: `session.execution.succeeded`, `.failed {error}`, `.interrupted`. `session.step.ended {tokens: {input, output, reasoning, cache: {read, write}}}` gives usage, and `session.retry.scheduled` gives rate limits. This needs a small bridge in the guest (`zapper-agent` turning the HTTP and SSE into lines), because the foreman speaks over a terminal channel. The SSE stream has no replay.

- **Sessions.** Ids look like `ses_…`; resume with `--session <id>` or `session/load`. They live in SQLite (`~/.local/share/opencode/opencode.db`, `OPENCODE_DB`), not in files.
- **Mid-turn mail.** A prompt's `delivery: "steer"` (at the next step) or `"queue"` (on the next turn) hands mail over natively, so `steers()` is true if the transport exposes it. There's no Stop hook: V2 plugin hooks cover prompt admission, model requests, retries, tools and permissions. `turn_end` can't be supported as a command, and the host's `queue` delivery replaces it.

**Open questions.**
- Whether ACP exposes `delivery` and token usage.
- How V2 reports a refused token.
- Which tools Code Mode leaves visible to the office's skills, which name tools like `mail_send`.

## Adding a harness

1. Add its name to `AGENT_KINDS` (`crates/zapper-policy/src/config.rs`) and to `harness::get`.
2. Implement `Harness`: `config_dir`, `skills_dir` if it isn't `<config_dir>/skills`, and `guest`, which writes its settings, instructions file and MCP config from the neutral keys and switches off update checks.
3. To run office workers, implement `Driver` and `Conversation` for its headless protocol, mapping its output to `Session`, `Activity`, `TurnEnd` and `Send`, and `steer` if it takes messages while it works.
4. Build an image that installs it at a pinned version, and a profile with `agent.kind` set to it.
5. Unit-test the mapping of its output (like `harness::tests::claude_output`), and for office workers add an e2e firm that uses it, with a fake that speaks its protocol or with the real agent.
6. Update [configuration](docs/configuration.md#agent-harnesses) and this page.
