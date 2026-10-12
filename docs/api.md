[← README](../README.md) · [Docs](README.md) · **API**

# API

## Overview

The daemon serves the API on its unix socket, mode 0600 in a 0700 directory; whoever can open the socket can use the API. Request bodies are YAML or JSON, and responses are JSON unless noted. Errors are `{"error": "…"}` with status 400 or 404. The read-only UI serves the GET routes marked **ui** under `/api` on its TCP address, behind its token.

```
curl --unix-socket $XDG_RUNTIME_DIR/zapper/api.sock http://zapper/v1/status
```

## Endpoints

| Method and path | ui | Purpose |
|---|---|---|
| `GET /v1/status` | ✓ | Version, session counts, unsaved changes, pending approvals |
| `GET /v1/config[?format=json]` | ✓ | Running config, YAML by default, secret values redacted |
| `PUT /v1/config` | | Replace the running config (validated; `<redacted>` keeps old values) |
| `GET /v1/config/startup` | ✓ | Startup file, normalised and redacted |
| `GET /v1/config/diff` | ✓ | Unified diff, startup → running (text) |
| `POST /v1/config/save` | | Write the running config to the startup file |
| `POST /v1/config/reload` | | Replace the running config with the startup file |
| `GET /v1/rules[?scope=S]` | ✓ | Rules with `scope`, `kind` and fields |
| `POST /v1/rules` | | `{scope, rule: {kind, …}, position?}` → the rule with its id |
| `DELETE /v1/rules/{id}` | | Remove a rule from any layer |
| `GET /v1/profiles` | ✓ | Summaries |
| `GET\|PUT\|DELETE /v1/profiles/{name}` | | Profile and its resolved form / set / remove |
| `GET /v1/sessions` | ✓ | Sessions |
| `POST /v1/sessions` | | `{profile, workdir, rules?}`; returns once the guest is ready |
| `GET /v1/sessions/{id}` | ✓ | One session |
| `POST /v1/sessions/{id}/stop` | | Stop, keep listed |
| `DELETE /v1/sessions/{id}` | | Stop and forget |
| `GET /v1/sessions/{id}/audit` | ✓ | Audit log (`application/x-ndjson`) |
| `GET /v1/sessions/{id}/attach?tty=RxC&argv=JSON&system=bool` | | Upgrade (`Upgrade: zapper-shell`) to a terminal; frames below |
| `POST /v1/evaluate` | | `{session?\|profile?, net?: {host, port, method?, path?}, exec?: {path, argv}}` → decision |
| `GET /v1/events[?session=ID&history=true]` | ✓ | Server-sent events, one JSON event per `data:` line |
| `GET /v1/approvals` | ✓ | Pending approvals with suggested rules |
| `POST /v1/approvals/{id}` | | `{allow, remember?: session\|profile}` |

## Terminal frames

After the upgrade, both directions carry frames: a 1-byte type, a 4-byte big-endian length, then the payload (at most 64 KiB).

| Type | Direction | Payload |
|---|---|---|
| 0 data | both | stdin to the guest; stdout (and stderr with a tty) from it |
| 1 stderr | guest → client | stderr without a tty |
| 2 resize | client → guest | rows u16, cols u16 |
| 3 eof | client → guest | stdin closed |
| 4 exit | guest → client | exit code i32; the last frame |

`system=true` runs as root without exec supervision. The CLI uses it for `zapper cp`.

## References

- Sources: `crates/zapper/src/daemon/api.rs`, `crates/zapper/src/client.rs`, `crates/zapper-policy/src/proto.rs`
