# The Office

The Office runs a firm of AI workers on your machine. A firm is an org chart: a CEO, a product manager, managers, engineers, researchers, analysts and security officers, each a Claude Code instance in its own Firecracker microVM. You are the board: you start an idle firm and give the CEO a task. Workers mail each other, wake when mail arrives, publish their work to a shared drive, push to the firm's git repository and deploy to the firm's own k3s cluster. Security officers watch the firm's traffic and decide access requests. Every run is journaled on disk and can be replayed.

> [!TIP]
> If you use a coding agent, point it at [AGENTS.md](AGENTS.md) and it will set The Office up. Operating a running daemon is covered by the [zapper skill](skills/zapper/SKILL.md).

## What you see

The web UI opens on the firm:

- **Overview**: the org chart with an animated avatar per worker (idle, working, waiting, queued, reviewing an access request), envelopes moving along the chart as mail is sent, and a side panel per worker to write to it as the board, see its desk and follow its current turn.
- **Messages**: every thread in the firm, the board's inbox, and the delegation tree of each request from the board.
- **Security**: standing approvals, every access decision with the officer's reason, escalations waiting for the board, and the firm's traffic.
- **Shared drive**: published documents, reports, decks, board packs, datasets and code.
- **Statistics**: activity, turns and tokens against the budget, mail, output and security, per worker and for the firm.
- **VMs**: every running VM, with its size and usage, read-only terminals and each worker's Claude transcript.
- **Runs**: past runs open in replay, with a timeline to play, pause and scrub.

The product the engineers deploy is served on a local port; until they deploy one, the cluster serves a landing page about the firm. The cluster's Kubernetes dashboard is on another local port. [docs/office.md](docs/office.md) describes all of it.

## Quickstart

On a Linux host with KVM, from a fresh clone (The Office uses `config/zapper.yaml` there, which defines the example firm `acme`):

```
make deps                     # Firecracker and a guest kernel into ~/.local/share/zapper (~1 min)
make install                  # zapper and zapper-agent into ~/.local/bin
make image                    # base Ubuntu image (~2 min)
make image IMAGE=office       # the workers' image: Claude Code (pinned), git, kubectl, podman (~5 min)
make image IMAGE=k3s          # the firm's cluster (~2 min)
make kernel KERNEL=k8s        # the cluster's kernel (~10 min)
claude setup-token            # on the host; save the token 0600 in ~/.config/zapper/claude-token
zapper check                  # each line "ok", or the command that fixes it
zapper serve                  # the daemon; prints the UI URL
```

In another terminal:

```
zapper firm up acme           # boots the cluster, then seven idle workers
zapper mail send acme --to barbara --kind assignment --subject "First product" --body "…"
zapper firm journal acme -f   # mail, states, turns, access requests, publishes
zapper firm down acme         # ends the run; it stays on disk for replay
```

Or open the UI, click the CEO and write the first task there. Workers use your Claude subscription; `budget` in the config limits turns per day. The [quickstart](docs/quickstart.md) walks through this step by step, [docs/commands.md](docs/commands.md) lists every command, and [docs/README.md](docs/README.md) indexes the documentation.

Local endpoints, all on loopback only:

| Address | What | Set by |
|---|---|---|
| `http://127.0.0.1:7878/?token=…` | The web UI: the office (org chart, messages, security, shared drive, statistics, VMs, replay). `zapper serve` prints the full URL with its token and writes it to `state/ui-url` | `daemon.ui` |
| `http://127.0.0.1:7878/sessions` | The sessions view: sessions, network, terminals, approvals | `daemon.ui` |
| `http://<host>:8080/` | The firm's product: the cluster's ingress (the landing page until the engineers deploy), on all interfaces so the LAN reaches it too | `firms.<firm>.product_port` |
| `http://127.0.0.1:8081/` | The cluster's Kubernetes dashboard (Headlamp); the UI has the sign-in token | `firms.<firm>.dashboard_port` |
| `$XDG_RUNTIME_DIR/zapper/api.sock` | The REST API for the CLI ([API](docs/api.md)) | `$ZAPPER_SOCKET` |

## Status

The Office is early software that works end to end. `make test-e2e` boots real VMs, checks each control from inside the guest, including attempts to get around the controls as root, and then runs firms of real VMs with a fake Claude. A firm with real Claude Code has delegated, published and reported back. [docs/office.md](docs/office.md#limitations) and [docs/security.md](docs/security.md#limitations) list the known limitations.

## Built on zapper

The office runs on zapper, which is the daemon, the `zapper` CLI and the sandbox each worker runs in:

- Each worker is a zapper session: a small Linux VM (Firecracker), started through a shim that confines the VMM in its own namespaces.
- Claude Code runs inside it as a normal user, with `bypassPermissions`.
- All the VM's network traffic is captured outside it and goes through zapper on the host. zapper answers its DNS, checks each connection against layered rules (session → profile → global), and makes allowed connections itself.
- Rules say allow, deny, or ask. In a firm, an `ask` becomes an access request for the security officers.
- The Claude token stays on the host: zapper adds it to requests to `api.anthropic.com`, so workers use it but can't read it.
- Each program a worker starts is checked by the host before it runs.
- Everything a session does is logged, and terminals are recorded.
- Rule changes apply at once, also to running workers; `zapper config save` keeps them.

The same sessions can be started on their own (`zapper run`), to check the setup or to debug a profile. Firecracker can't pass devices through, so there is no GPU work inside the VMs.

More: [architecture](docs/architecture.md), [network](docs/network.md), [policy](docs/policy.md), [security](docs/security.md).

## Next

1. **Resuming a firm's run** after a daemon restart, with persistent worker disks and conversations.
2. **Network rate and volume limits** per session: bandwidth and packet rate on the NIC, using Firecracker's rate limiter, and byte counts per destination and per session in the proxy, with an event when a limit is hit.
3. **UDP egress** where a rule allows it, for QUIC and NTP.

## License

Apache-2.0
