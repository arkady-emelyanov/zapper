---
name: zapper
description: Operate a running zapper daemon, which runs agents, interview candidates and study sandboxes in policy-bound Firecracker microVMs. Use when the user wants to start, attach to, inspect or stop a sandbox session; allow or block a host or command for a session or profile; answer a pending approval; see what a session did (network, execs, exits, terminal recording); change, diff, save or reload the zapper config; build an image; or run Claude Code in a sandbox.
---

# Operating zapper

The daemon (`zapper serve`) owns the sessions; the `zapper` CLI talks to it over a unix socket. Before acting, find out what is running and whether the config has unsaved changes:

```
zapper ps                 # sessions: id, profile, state, counters
zapper config diff        # unsaved running-config changes ("no unsaved changes")
zapper approvals          # anything waiting for a decision
```

If the CLI says it can't connect, the daemon isn't running: start it with `zapper serve` in the background and a log (`zapper serve > /tmp/zapper.log 2>&1 &`), then give the user `tail -f /tmp/zapper.log`.

## Rules

- Sessions may belong to someone else, such as a candidate in an interview. Before `stop`, `rm`, `config reload` or restarting the daemon, check `zapper ps` and say what you're about to end. A restart stops every session and loses unsaved config.
- API changes edit the running config only. Run `zapper config save` when the user wants a change kept, and show `zapper config diff` first.
- Prefer the narrowest change: a session rule (`session:ID`) over a profile rule, and a profile rule over a global one. Prefer `methods`/`path` limits over allowing a whole host.
- Keep secret values out of rules, profiles and commands. Secrets come from `env` or `file` sources in the config: `proxy` secrets are bound to `hosts`, `guest` secrets are delivered into the session. The user creates the files; don't read or print them.
- Don't approve requests on the user's behalf unless they said to. Show the pending request and ask.
- Audit logs and recordings are sensitive: summarise them rather than pasting them wholesale.

## Sessions

```
zapper run                               # shell on a copy of $PWD, removed on exit
zapper run -p claude -C ~/proj --keep    # keep it after the command exits
zapper attach ID                         # another terminal (profile command, or -- CMD)
zapper exec ID -- git -C /work status    # no terminal; exit code passes through
zapper cp ID:/work ./out                 # results out (mounts are copies)
zapper stop ID ; zapper rm ID
```

## Rules and approvals

```
zapper rules --scope profile:claude
zapper rule add session:ID '{kind: net, action: allow, host: pypi.org, ports: [443], methods: [GET]}'
zapper rule add profile:claude '{kind: exec, action: deny, path: ssh}'
zapper rule add global '{kind: net, action: deny, host: "*.example.internal", final: true}'
zapper rule rm net-7
zapper eval -s ID net github.com:443 POST /org/repo/pulls      # which rule decides, before changing anything
zapper approve ABC --remember session    # or: zapper deny ABC
```

## What happened

```
zapper events --history -s ID            # live and recent, one line each
zapper audit ID | jq -r 'select(.type=="net" and .action=="deny") | "\(.host) \(.error // "")"' | sort | uniq -c
zapper audit ID | jq -r 'select(.type=="exec") | .argv | join(" ")'
ls ~/.local/state/zapper/sessions/ID/    # audit.jsonl, shell-N.cast (asciinema play), console.log
```

The read-only UI URL is in `~/.local/state/zapper/ui-url`.

## Config

```
zapper config show | diff | save | reload | edit
zapper profile ls ; zapper profile show claude
```

## Images

```
zapper image ls
make image IMAGE=claude > /tmp/zapper-image.log 2>&1     # background; tail -f the log
```

## More

- Rules and secrets: [docs/policy.md](../../docs/policy.md)
- Config keys and profiles: [docs/configuration.md](../../docs/configuration.md)
- Security model: [docs/security.md](../../docs/security.md)
- Every command: [docs/commands.md](../../docs/commands.md)

## Installing this skill

Claude Code loads it from this repository through `.claude/skills/zapper`. To use it elsewhere, copy `skills/zapper/` to `~/.claude/skills/zapper/`; other agents can read this file directly.
