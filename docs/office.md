[← README](../README.md) · [Docs](README.md) · **The Office**

# The Office

> [!NOTE]
> The office is new and works end to end: `make test-e2e-office` runs a firm of real VMs with a fake Claude, and a firm with real Claude Code has delegated, published and reported back. What it doesn't do yet is under [limitations](#limitations).

## Overview

A **firm** is a set of AI workers, each a Claude Code instance in its own zapper session. An org chart in the config says who they are, who reports to whom and what each is told to do. Workers talk by **mail**; a new message wakes an idle worker with the context of what is asked. Finished work is **published** to the firm's **shared drive**. **Security officers** are workers that watch the firm's traffic and decide access requests. The operator is the **board**: they start an idle firm, give the CEO a task, and watch the work in the web UI.

The office runs on zapper, which provides the sandbox: each worker is a zapper session with a profile, its traffic goes through zapper's proxy and policy, its execs are checked, and everything is recorded ([architecture](architecture.md), [policy](policy.md), [security](security.md)).

## Concepts

| Term | Meaning |
|---|---|
| Firm | One org chart and everything it produces. Defined under `firms:` in the config. Several can exist; usually one runs. |
| Worker | One named member of the firm, one VM, one Claude Code. Names are unique in the firm and are its mail address (`barbara`, `dennis`, `ada`, `radia`). |
| Role | A template for workers: skills, profile, model, avatar. `ceo`, `product-manager`, `manager`, `engineer`, `designer`, `researcher`, `analyst`, `security-officer`, or one defined in the config. |
| Board | The operator. Not a VM; an address (`board`) that can mail anyone and receives reports. |
| Mail | A message from one address to others, in a thread, with optional attachments from the shared drive. |
| Desk | A worker's open work: threads assigned to it, threads where it waits for a reply, pending access requests. |
| Shared drive | The firm's published work: versioned artifacts with a title, kind, author and audience. Every worker can publish to it. |
| Foreman | The part of the daemon that drives one worker's Claude Code: delivers mail, starts turns, tracks idle/working. |
| Facility | A VM in the firm with no Claude in it that provides a service to workers, such as the Kubernetes cluster. |
| Product | What the firm builds and deploys to its cluster; the board reaches it on `product_port`. |

## Configuration

The repository's config ([config/zapper.yaml](../config/zapper.yaml)) has a firm `acme` and its profiles. In short:

```yaml
firms:
  acme:
    description: A small product company
    board: { name: Board }                 # the operator; mail to `board` lands in the UI
    budget: { daily_turns: 400, max_concurrent: 3 }
    git: {}                                # an empty repository per run at https://git.acme.internal/product.git
    product_port: 0.0.0.0:8080             # browsers on the LAN → the cluster's ingress; 127.0.0.1:8080 for this machine only
    facilities:
      cluster: { kind: k3s, profile: office-k3s }   # image k3s, kernel vmlinux-6.1-k8s
    landing: { name: "Acme Inc.", motto: "Building digital worlds!" }   # served until the product is
    credentials:                           # handed to these roles' VMs at boot
      kubeconfig: [engineer]
      git: [engineer]
    roles:
      ceo:
        profile: office                    # a zapper profile: image, network, secrets
        title: CEO
        skills: { work: ceo }              # communication: office-mail, publishing: office-publish by default
        avatar: { figure: ceo, color: "#D97757" }
      engineer:
        profile: office-engineer           # also reaches git and the Kubernetes API
        model: claude-sonnet-5-5           # others use the profile's claude-opus-5-5
        skills: { work: software-engineer }
        avatar: { figure: engineer, color: "#6A9BCC" }
      security-officer:
        profile: office-security           # wide network, no secrets
        skills: { work: security-review }
        avatar: { figure: shield, color: "#788C5D" }
      # product-manager, manager, researcher, analyst …
    workers:
      - { name: barbara,  role: ceo }
      - { name: margaret, role: product-manager, reports_to: barbara }
      - { name: dennis,   role: manager, reports_to: barbara, title: CTO }
      - { name: ada,      role: engineer, reports_to: dennis }
      - { name: grace,    role: engineer, reports_to: dennis }
      - { name: iris,     role: designer, reports_to: margaret }
      - { name: ken,      role: analyst, reports_to: margaret }
      - { name: radia,    role: security-officer, reports_to: barbara }
    security:
      officers: [radia]
      digest_every: { minutes: 10, requests: 200 }
      answer_within: 15m                   # then the board is asked too
```

| Field | Meaning |
|---|---|
| `domain` | Names the firm's services; default `<firm>.internal` |
| `budget` | `daily_turns`, `worker_daily_turns`, `daily_tokens` (0 is no limit), `max_concurrent`; days end at midnight UTC |
| `limits` | `thread_messages` (60), `mail_per_hour` (120), `fanout_per_hour` (10), `pair_per_10min` (30), `mail_per_run` (20000), `artifact_size` (50MiB), `drive_size` (2GiB) |
| `roles.<r>` | `profile`, `model` (default: the profile's), `title`, `skills: {work, communication, publishing}`, `avatar: {figure, color}`, `fake` (a fake Claude script, for tests) |
| `workers` | `name`, `role`, `reports_to`, `title` |
| `security` | `officers`, `digest_every: {minutes, requests}`, `answer_within` |
| `git`, `facilities`, `credentials`, `product_port`, `dashboard_port`, `landing` | The firm's services ([below](#facilities-the-cluster-git-and-the-product)) |
| `skills_dir` | A directory of `<name>/SKILL.md` used instead of the built-in skills of the same name |

The office adds to each worker's profile at boot: the preamble in `~/.claude/CLAUDE.md`, the skills, the `office` MCP server, the model, and credentials for the firm's services. The profile still decides the image, the VM's size, the network rules and the secrets.

### What a worker knows at the start

Only its skills and who it is. A worker boots with its three skills and a short generated preamble in `CLAUDE.md`: its name, title, manager, direct reports, and the firm's services it can reach ([prompts](office-prompts.md#office-preamble)). Nothing in the config says what the firm builds. The product comes from the board's first message to the CEO, and from there through the CEO's and the product manager's decisions, mailed down as assignments and published as documents on the shared drive. Two runs of the same firm with different kickoff messages build different products.

### Validation

Rules: names are unique in the firm and can't be `board`, `all`, `team`, `security` or `office`; `reports_to` forms a tree with one root; every role names a profile that exists; each worker gets the three skills. Profiles used by firm roles can't have `exec` rules with `ask`: officers decide network access only, and a paused exec can't wait minutes for them. The config is validated as a whole like the rest of zapper's config.

The org chart is fixed when the firm boots. Workers, roles, `reports_to` and skills are read at `firm up`; changing them in the running config is allowed and applies at the next `firm up`. Network and exec rules still apply to running workers at once, as for any session.

### Resources

Each worker is a VM sized by its role's profile. The example firm (six workers at 4 GiB, two engineers at 6 GiB, the cluster at 8 GiB) needs about 44 GiB of RAM. `firm up` checks the total against `daemon.max_sessions` and the host's free memory before booting anything, and refuses with the numbers if it doesn't fit.

## Messaging

### Where mail lives

The daemon is the post office. Mail is written to the run's journal on disk and the daemon keeps only an index in memory ([state](#state)). Workers never hold other workers' mail; they ask the post office for theirs. This keeps identity honest: a worker can only send as itself, because the daemon knows which VM a request came from.

### How a worker reaches the post office

A vsock port, **office**, in every worker VM. The guest connects to the host on it; the daemon knows the VM from the socket the connection arrived on, so no token is needed and none can be stolen. It works with or without a NIC and doesn't touch the network policy.

In the guest, `zapper-agent office` is two things:

- An **MCP server** (stdio) that Claude Code loads through `mcpServers`. Its tools are what the communication and publishing skills teach.
- A **CLI** (`office send`, `office inbox`, `office publish`) for scripts and for humans attaching to a worker.

The MCP server offers only the tools of the worker's role: officers get the traffic and decision tools, other workers don't. Workers run with `bypassPermissions` and can open the office port themselves, so the daemon also checks the caller's role on every request; the MCP scoping is what the worker sees, the daemon's check is what holds.

### The office protocol

As simple as it can be: one JSON request and one JSON response per connection.

```
guest → host   {"op": "mail_send", "args": {"to": ["ada"], "subject": "…", …}}\n
host → guest   {"ok": {"id": "m-01J…", "thread": "t-01J…"}}\n      or {"error": "…"}\n
```

- Each MCP tool is one `op` with the same arguments; the MCP server and the CLI are thin translators.
- A request is one line of at most 128 KiB; the daemon closes the connection after its answer.
- No files travel on this port. `drive_publish` names paths in the VM, and the daemon copies them out itself over the system terminal channel, the way `zapper cp` does; `drive_fetch` copies into the VM the same way, owned by the worker's user. So the protocol has no binary framing, and the daemon reads only what was named.
- Nothing is pushed to the guest on this port: new mail reaches a worker as a wake prompt from the foreman.

### Message format

```
id          m-01J…              unique, time-ordered
thread      t-01J…              first message's id; replies keep it
in_reply_to m-…                 optional
from        dennis
to          [ada]               names, `board`, `team:dennis` (direct reports), `all`
cc          [iris]
subject     "Landing page v1"
body        markdown, ≤ 64 KiB
attachments [drive:a-17@v2]       shared drive references, never raw files
kind        assignment | question | reply | report | fyi | follow-up (sent only by the post office)
due         optional timestamp; past it, the post office follows up
expects     reply | done | nothing
follow_up   optional: {every, max}; defaults below
```

`kind` and `expects` let the receiver and the UI tell work apart from chatter: an `assignment` stays on the receiver's desk until they mark it done or reply with `report`.

### Who may mail whom: open talk, top-down authority

Like a real organisation, anyone may write to anyone: an engineer can ask the CFO a question, mail a researcher in another team, or ask a security officer for access. Authority, though, flows down the chart, and the post office enforces it rather than leaving it to the skills:

| Kind | Who may send it |
|---|---|
| `assignment` | the board to anyone; a worker to its direct and indirect reports |
| `question`, `reply`, `report`, `fyi` | anyone to anyone |
| access decisions | security officers and the board only |

An `assignment` from someone without authority over the receiver is rejected with an error that says to send a `question` or ask the receiver's manager. A request to a security officer is a request; only the officer's `access_decide` or the board changes the rules. No worker, the CEO included, can override an officer's decision or the policy's `final` rules; only the board can. Managers can reprioritise or cancel assignments they or their reports gave.

### MCP tools (communication)

| Tool | Does |
|---|---|
| `mail_send(to, cc?, subject, body, kind, expects?, due?, attachments?)` | Start a thread |
| `mail_reply(message, body, kind?, attachments?, close?, all?)` | Reply in a thread, to the sender (`all`: everyone in it); `close` marks the assignment done |
| `mail_inbox(unread?)` | List messages with sender, subject, kind, thread |
| `mail_read(message)` | One message with its body |
| `mail_thread(thread)` | Full thread, oldest first |
| `desk()` | My open assignments, threads waiting on others, pending access requests |
| `directory()` | The org chart: names, titles, roles, who reports to whom, who is busy |
| `request_access(host, port?, methods?, path?, reason)` | Ask the security officers ahead of time, with a reason |

There is deliberately no blocking `wait_for_reply`. A worker that needs an answer sends its question, notes it on its desk, and ends its turn; the reply wakes it. This avoids burning tokens while sleeping and avoids deadlocks between workers waiting on each other.

### Follow-ups

A message with a `due` time and `expects: reply` or `done` gets an automatic follow-up if the receiver hasn't answered by then. The post office sends it in the same thread, on behalf of the original author: it shows as from the author, is marked `kind: follow-up` and `automatic: true`, quotes the original request and says how overdue it is. It wakes the receiver like any other mail, and its wake prompt puts the overdue request first.

The author isn't woken and spends no tokens; their desk shows the thread as overdue. If there is still no answer, the post office follows up again after the `follow_up` interval (default: the time between sending and `due`, at least 15 minutes). After the last follow-up (default 2) it escalates: the receiver's manager gets a `fyi` that the request is stuck, and the author is woken with a note, so they can wait, reassign or ask someone else. A reply or a `close` from the receiver cancels pending follow-ups.

Follow-ups for messages from the board work the same way, so a CEO that hasn't answered the board is reminded on the board's behalf. The board can turn this off per message.

### Waking an idle worker

This is the core problem. Claude Code finishes a turn and then sits idle; nothing inside it watches a mailbox.

**The foreman drives Claude Code headless.** For each worker the daemon runs, inside the VM over the existing terminal channel:

```
claude -p --input-format stream-json --output-format stream-json --verbose
```

That process stays alive and reads user messages from stdin. The foreman:

1. Writes a user message (a **wake prompt**) when mail arrives and the worker is idle.
2. Reads stream-json events. A turn starts with the user message and ends with the `result` event: that is exactly the idle/working signal the avatars need, plus token usage for budgets.
3. While the worker is working, new mail is queued. At the end of the turn all queued mail is delivered in one wake prompt, most urgent first (board, then manager, then others).
4. If the process exits or crashes while the VM is up, the foreman restarts it with `--resume <session-id>` so the conversation continues.
5. Writes every stream-json event to the worker's **transcript** journal on disk as it arrives. The UI's live view and the replay read from there; the daemon keeps no transcripts in memory.

The wake prompt gives context and scope:

```
New mail (1 of 1) — thread "Landing page v1" (t-01J…)
From: dennis (CTO, your manager) · kind: assignment · due: today 18:00 · expects: done
Earlier in this thread: none.

<body of the message>

Attachments: drive:a-17@v2 "Brand guide" (fetch with drive_fetch)

Your desk: 1 open assignment (this one), 0 threads waiting on others.
When finished: publish the result, then mail_reply with kind=report and close=true.
```

Long threads are summarised: the last N messages verbatim, older ones as one line each. The desk is in every wake prompt so a worker whose context was compacted still knows what it owes whom.

Rejected alternatives: typing into an interactive TUI through the terminal (fragile, no clean turn boundary); a `Stop` hook that refuses to stop while mail is unread (can't wake an already idle process); a blocking wait tool (tokens and deadlocks). Mail that arrives during a turn waits for the turn to end ([limitations](#limitations)).

**Conversation scope: one per worker.** Each worker has one long Claude conversation, kept by Claude Code's compaction plus the desk in every wake prompt. This fits a worker that works on its own but asks for clarification: the engineer reads the assignment, starts, sends the manager a `question`, and ends its turn; the reply wakes the same conversation, which still holds the half-done work and the reasoning behind the question. It also lets managers remember everything they delegated, and lets a worker carry lessons from one assignment to the next. The alternative, a fresh conversation per assignment, isolates tasks better but forgets across them; it can be added later as a per-role option if workers mix tasks up.

### Guards against runaway firms

- **Budgets**: turns and tokens per worker per day and per firm; at the limit a worker sleeps and the board gets a notice.
- **Concurrency**: at most `max_concurrent` workers in a turn at once; the rest queue. All workers share one Claude subscription token, so the plan's rate limits are the real ceiling: the default is 3, and a turn refused for rate limits is retried with backoff rather than counted as a failure.
- **Loops**: a thread of `thread_messages` messages (60) is paused, and only the board can still write to it. Two workers who exchange `pair_per_10min` messages (30) within ten minutes can't mail each other until the window moves, and the board gets one notice per pair per hour.
- **Fan-out**: `all` and `team:` mail are limited per worker per hour.
- **Size**: body, attachments per message, mail per worker per hour, all bounded like everything else from the guest.

## Publishing

### The shared drive

The firm's shared drive is a versioned artifact store in the run's directory on disk ([state](#state)). Files are stored by content hash; an artifact is metadata pointing at them.

```
id        a-17
version   2                  each publish of the same artifact adds a version
title     "Pricing analysis"
kind      document | report | code | dataset | deck | board-pack
author    ken
summary   one paragraph, shown in lists and in mail
changes   what this version changed from the previous one; required from version 2
files     [{path: pricing.md, sha256, size, type}]
audience  firm | board | [names]
thread    t-01J…             the assignment it answers, if any
```

### MCP tools (publishing)

| Tool | Does |
|---|---|
| `drive_publish(paths, title, kind, summary, audience?, artifact?, thread?, changes?)` | Copy files from the VM into the shared drive; with `artifact`, add a new version, which needs `changes`: what changed from the previous one |
| `drive_list(kind?, author?, query?)` | Find artifacts |
| `drive_fetch(artifact[@version], dest)` | Copy an artifact into the VM |

Publishing copies the named files out of the VM (see [the office protocol](#the-office-protocol)): Firecracker can't share folders, and copies mean a worker can't change something after publishing it. Limits: 50 MiB per artifact and 2 GiB per run, both configurable.

### Kinds and what the UI does with them

- **document, report**: Markdown, rendered in the UI.
- **deck, board-pack**: Markdown slides (one `---` per slide) plus images; the UI shows them as a slide viewer. The CEO's publishing skill says a board pack has a summary, progress against the goal, metrics, risks and asks.
- **dataset**: CSV/JSON with a table preview.
- **code**: a tree of files, or a git bundle. The product's code lives in the firm's [git repository](#the-git-repository); a `code` artifact is for snapshots and for code outside the product.

### Rendering safely

Everything a worker publishes is untrusted. The UI keeps its rule of building pages from text, with a small Markdown renderer written for it (the UI has no dependencies besides xterm.js):

- Supported: headings, paragraphs, emphasis, inline code and code blocks, lists, block quotes, tables, horizontal rules (slide breaks in decks), links and images.
- Every node is made with `createElement` and filled with `textContent`; raw HTML in Markdown is shown as text, never parsed.
- Links show their target and open in a new tab with `rel=noopener noreferrer`; only `http`, `https` and `drive:` links are made clickable.
- Images may only point at files of the same artifact. They are served from a separate path with their own content type and `Content-Security-Policy: sandbox`, and shown with `<img>`, so SVG scripts don't run.
- Input size and nesting depth are bounded, and a test page of hostile Markdown is part of the UI tests.

Mail bodies use the same renderer.

## Security officers

A security officer is a worker whose profile allows wide internet access (everything except private networks, the host and cloud metadata, which `final` rules keep closed) and holds no secrets. It does two jobs: it **watches all the firm's traffic**, and it **decides access requests**, turning the good ones into standing approvals so the same request doesn't need deciding again.

### Watching traffic

Officers see every worker's network activity: the request log zapper already keeps (method, URL, status, sizes, which rule decided), DNS queries and refusals, with added secret values shown as `***`. The foreman wakes an officer:

- at once, for each `ask` (a request waiting on a decision);
- periodically, with a digest of what was allowed since the last one (every few minutes or every N requests, whichever comes first), so the officer can spot unusual traffic: a new host, a burst of uploads, a worker reaching something unrelated to its assignment.

Tools: `traffic(worker?, host?, since?, limit?)` to query the log, `traffic_digest()` for the latest summary, `access_requests()` for what is pending, `access_rules()` to list what officers have approved so far, with how many requests each allowed, `access_decide(request, allow, scope, reason)`, `access_revoke(rule, reason)`, and `escalate(request, recommendation)` for what only the board can decide.

When something looks wrong the officer can revoke a standing approval, deny a host for a worker or role, mail the worker or its manager to ask why, or mail the board.

### Deciding requests

1. A worker's request hits an `ask` rule, or the worker asks ahead with `request_access(host, …, reason)`. It goes to the officers' queue. The request is **refused at once**, not held: officers take minutes, and most clients time out long before that. An HTTP request gets a 403 whose body says access was requested from security and gives the request id; a connection that can't carry a body (passthrough TLS, plain TCP) is closed. The request shows on the worker's desk, and repeated attempts at the same thing while it's pending join it rather than adding new ones.
2. The foreman wakes an officer with the request: worker, host, port, method, path, the worker's current assignment and its reason, if any.
3. The officer researches if needed (opens the site, checks what it is) and calls `access_decide(request, allow, scope, reason)`, where `scope` is:
   - `once`: this request only;
   - `worker` or `role`: a **standing approval**, a rule at the worker's or role's layer, optionally narrowed to methods and paths and optionally with an expiry.
4. The daemon applies it with the officer's name and reason, emits an event, and mails the worker the decision in the request's thread. That mail wakes the worker, which retries. Standing approvals belong to the running firm and end with it; the board sees them in the UI and can revoke them, or promote one into the running config, where `zapper config save` keeps it.

### Limits that hold whatever an officer decides

- Officers add rules only at worker and role layers, never global, and never past a `final` rule. The board can override an officer; no worker can, the CEO included.
- Secrets, private networks and new profiles stay with the board: an officer can recommend them, and the request is escalated with that recommendation.
- A worker's reason text is untrusted input to the officer; the officer's skill says so and shows it quoted.
- If no officer answers within `security.answer_within` (default 15 minutes), the request goes to the board as well, and stays pending until one of them answers or the firm stops. Nothing is held meanwhile, so there is no deny-on-timeout.
- Officers decide network access only. Exec rules in firm profiles are `allow` or `deny`, never `ask`. So the officer's decision on a download is in effect the decision on running it: if an officer allows a worker to fetch a tool, the worker can run it unless a profile `deny` rule says otherwise. Every exec is still checked against those rules and recorded, and officers see the download in the traffic log.
- Officers can't approve their own requests, and two officers (if configured) can see and revoke each other's approvals.

## Facilities: the cluster, git and the product

A firm builds and runs a product; which product is up to the board's kickoff and the CEO's and product manager's decisions. For that it has a git repository and a Kubernetes cluster, both new and empty in every run.

### Names and who reaches what

Facilities are reached by name under the firm's `domain`, through the same proxy and policy as everything else. The guest's resolver answers these names only when a rule could allow them, as for any host; they are never looked up on the host's DNS. The daemon routes connections to them itself, so no VM ever reaches another VM or a host address directly.

| Name | Port | What | Engineers | Other workers | Security officers |
|---|---|---|---|---|---|
| `git.acme.internal` | 443 | The firm's git repository, served by the daemon | ✓ | | ✓ |
| `cluster.acme.internal` | 6443 | The Kubernetes API | ✓ | | ✓ |
| `apps.acme.internal`, `*.apps.acme.internal` | 80, 443 | The cluster's ingress load balancer: the product | ✓ | ✓ | ✓ |

The table is written as `net` rules in the role profiles of the example firm (engineers and officers are allowed those hosts, others only the ingress), so it can be changed like any rule. Officers can't widen it for a worker: facility names count as private, which stays with the board.

### The cluster

A facility VM running k3s on one node, booted first by `firm up`. Workers boot once its API answers.

- **Image** `k3s`: Ubuntu with a pinned k3s release (version and checksum in the Makefile) and its air-gap image bundle, so the cluster starts without the internet. Traefik is the ingress and k3s's service load balancer binds it to ports 80 and 443 on the node; flannel uses `host-gw`.
- **Kernel** `vmlinux-6.1-k8s`. Firecracker boots one kernel file from the host, not a kernel from the image, and the standard one is built without loadable modules, so nothing can be installed into it. Kubernetes needs netfilter matches (`comment`, `statistic`, `multiport`, `mark`, `addrtype`), ipset and more cgroup controllers, so `make kernel KERNEL=k8s` builds the same 6.1 source and config with a fragment that adds them (`config/kernel/k8s.config`), inside podman, once, in about 10 minutes. `k3s check-config` passing in the VM is an e2e check. Worker VMs keep the standard kernel.
- **Inbound**: the agent in the cluster VM forwards vsock ports to `127.0.0.1:6443`, `:80` and `:443`. Only the host (CID 2) can connect, as with terminals; the daemon connects when a worker's allowed connection to a facility name arrives.
- **Egress**: the cluster pulls images through the proxy like any session. The example profile allows the common registries on port 443 (`registry-1.docker.io`, `auth.docker.io`, Docker's blob host, `ghcr.io` and its blob host, `quay.io`); anything else asks the officers. The air-gap bundle includes busybox, which needs no pull. The request names the facility instead of a worker. Kubernetes retries pulls by itself, so an approval takes effect without anyone being woken.
- **No shell for workers**: engineers get the API and the ingress, nothing on the node. The board can `zapper exec --system` into the cluster VM.
- **A dashboard for the board**: with `dashboard_port: 127.0.0.1:8081`, Headlamp (pinned in the k3s image, no pull) runs in namespace `dashboard` on a NodePort that only that local port reaches; workers can't. The UI's Overview and the cluster's card in the VMs tab show a **Kubernetes dashboard** button and **Copy token**, a cluster-admin service-account token of this run to sign in with.
- **A landing page from the start**: with `landing: { name: "Acme Inc.", motto: "Building digital worlds!" }`, the cluster serves a static page about the firm as soon as Traefik is up, so the product port shows something before anyone has built anything. It runs in namespace `landing` from the air-gap images (no pull), behind an Ingress on `/` at Traefik's lowest priority: any Ingress the engineers add on `/` takes over without removing it. The name defaults to the firm's name with "Inc.".

### Credentials

Members of technical staff wear several hats (engineering, devops), so engineers get real credentials in their VMs:

- **kubeconfig**: after k3s is up, the daemon reads `/etc/rancher/k3s/k3s.yaml` from the cluster VM, sets the server to `https://cluster.acme.internal:6443` (in the certificate's names through `--tls-san`) and writes it to `~/.kube/config` (0600) in each VM of a role listed under `credentials.kubeconfig`. It is cluster-admin. The proxy passes connections to the API through without inspection, so k3s's own CA works.
- **git**: a username and password per worker, made at `firm up`, written to `~/.git-credentials` with `credential.helper store` and `user.name`/`user.email` set to the worker's name. The git server checks the password and that the connection comes from that worker's VM. Engineers can push; officers can read.

How the product is built and deployed (images built in the VMs, a registry in the cluster, in-cluster builds, plain manifests) is up to the engineers. Their skill describes the environment they have (the repository, the cluster, the ingress, the product port's default route), not what to build or how.

### The git repository

A bare repository in the run directory, `runs/<id>/git/product.git`, empty at the start of each run. The daemon serves it over HTTPS at `git.acme.internal` by running `git http-backend` for each request, with the session's CA (which each VM already trusts, for inspected connections). It is part of the run's record: replay shows the commit history as it was, and `git clone` from the run directory works after the run.

### Ports on the host

Ports on the host:

| Port | What |
|---|---|
| `daemon.ui` (7878) | The firm: the web UI, on loopback |
| `product_port` (8080) | The product: each connection is forwarded to the cluster's ingress on port 80. Any address: `0.0.0.0:8080` serves it to the LAN, `127.0.0.1:8080` to this machine only |
| `dashboard_port` (8081) | The cluster's dashboard; loopback only, as it signs in as cluster-admin |

The product port forwards bytes and keeps the browser's `Host` header (`localhost:8080`), so the product must answer on the ingress's default route; the engineers' skill says so. Connections to it aren't checked against firm policy, as the board's own, but each one is logged with its peer address. On the LAN, anyone who can reach the host can use whatever the engineers deployed, so serve it there only on a network you trust; the host's firewall may also need to allow the port.

## Skills

Each worker gets three Claude Code skills. The firm's skill directories go on a disk attached read-only (zapper's `mode: ro` mount), and `~/.claude/skills/<name>` links into it, so a worker can't edit its instructions:

| Skill | Shared? | Teaches |
|---|---|---|
| Communication (`office-mail`) | all workers | Addresses and the org chart, message kinds, when to delegate vs do, replying with a `report`, never waiting in a loop, writing for the reader (subject says the ask, body starts with it) |
| Work (per role) | per role | The job: CEO sets direction and delegates; product manager turns goals into requirements and priorities; manager splits and reviews; engineer builds and tests; designer shapes flows, screens and words, and reviews what is built; researcher sources and cites; analyst models and shows numbers; officer reviews access |
| Publishing (`office-publish`, variants) | per role | What to publish, kinds, titles and summaries, versions instead of new artifacts, linking the thread, the board-pack format for the CEO |

Skills live in [config/skills/](../config/skills/). They are built into the binary, and the acme firm reads them live through `skills_dir: skills` (relative to the config), so an edit applies at the next `zapper firm up`. A skill missing from `skills_dir` falls back to the built-in one. Keep the frontmatter (`name`, `description`): Claude Code uses the description to decide when to read a skill.

## Web UI

The UI has a tab bar for the office. zapper's own views of sessions (network, terminals, approvals) are a separate page, `/sessions`, linked from the footer and from each worker's panel; its footer leads back to the firm.

| Tab | For |
|---|---|
| Overview | The firm at a glance, and writing to any worker |
| Messages | All mail in the firm |
| Security | What officers approved, what they decided, and why |
| Shared drive | Published work, including board packs |
| Git | The firm's repository: branches, files and commits |
| Statistics | Activity, throughput and budgets |
| VMs | Every running VM of the daemon, now: the host, each firm's workers and cluster, other sessions; size, usage and counters; terminals to watch read-only, and each worker's Claude transcript |

A header above the tabs shows the firm's name and state (off, starting, running, paused), unread board mail, escalations waiting for the board, and the firm controls: up, down, pause. A **Runs** menu in the header opens a past run in [replay](#replay); the header then shows a timeline instead of the controls.

A footer on every tab shows the run's vital numbers, and flashes one when it changes: workers working, waiting and idle; messages and open obligations (overdue in red); access requests pending and decided; artifacts and versions on the shared drive; commits on `main`; turns and tokens; time since the run started. Each opens its tab.

The UI's state is built by folding the run's journal in the browser: live, it follows the journal as it grows; in replay, it folds up to the timeline's position. One code path serves both.

### Overview

- **Org chart**: the tree from the config, top to bottom. Each node is the worker's animated avatar, name, title and a one-line status ("drafting pricing.md", "waiting for ada", "overdue: 1"). Envelopes animate along the edges as mail is sent, also between workers who aren't directly connected; a badge shows unread mail. The board sits above the CEO as its own node.
- **Click a worker** to open a side panel:
  - **Write as the board**: subject, body, kind (`assignment`, `question`, `fyi`), `due`, attachments from the shared drive or small files from the board's computer (up to 10 files, 5 MiB together, stored on the shared drive as the board's artifact). Sent as `board` in a new thread, or as a reply in one of the worker's threads.
  - **Desk**: open assignments, threads waiting on others, overdue items, pending access requests.
  - **Now**: a live transcript of the current turn (text and tool calls, rendered as text), and earlier turns on demand.
  - Recent mail, published work, and a link to its session in zapper's session views.
- **Kickoff**: when the firm is idle and has no threads, the CEO's panel opens with an empty assignment ready to write.

### Messages

A mail client over the whole firm, read-only except for the board's own mail.

- **Threads list** with subject, participants, kind and last activity. A thread shows a state only while it needs something: **waiting for** whoever owes an answer, **overdue** when that is late, **paused** by the loop guard. When the answer comes, the tag goes, in the list, the thread and on the message that asked.
- **Filters**: **from** (who wrote a message) and **to** (who received it, as recipient or in copy), each the board, a worker or the post office; kind, state and text search. From, to and kind have to match the same message: "from ada, to dennis" lists the threads where ada wrote to dennis. With these filters, each thread says how many of its messages match, opens at the first one, and the matching messages are highlighted.
- **Thread view**: messages oldest first, with sender avatars, kinds, `due`, attachments that open in the Shared drive tab, and follow-ups marked as automatic. While someone owes an answer, the thread says who owes what to whom, and by when.
- **Board inbox**: the threads the board is in, first, with reply and compose. Every live thread the board is in has a reply form, also before anyone has answered. The latest board pack is pinned at the top.
- **Delegation tree**: for any thread from the board, the threads it spawned (CEO → CTO → engineers) as a tree, with each branch's state, so the board can see how its request was split up and where it is stuck.

### Security

- **Standing approvals**: every rule an officer added at a worker or role layer: host, ports, methods, paths, scope, expiry, officer, reason, when, and how many requests it has allowed since. The board can revoke one or promote it into the startup config.
- **Decision log**: every access decision, newest first: the request (worker, host, method, path), the worker's reason, the assignment it was working on, the officer, allow or deny, scope, reason, time to decide. Filters by worker, officer, host, outcome. Expanding an entry shows the request in the network view and the officer's turn transcript for that decision.
- **Escalations**: requests waiting for the board (secrets, private networks, global rules), with the officer's recommendation; answering here is the board's decision.
- **Traffic**: the firm's requests across all workers as one live list (allowed, denied, waiting), the officers' digests, and anything an officer flagged.

### Shared drive

Artifacts by kind, author and thread. An artifact shows its version history, newest first, each version with the author's note of what changed; clicking one opens that version. Viewers: Markdown for documents and reports, a slide viewer for decks and board packs, a table for datasets, a file tree for code.

### Git

The run's repository, read-only: a branch picker (`main` first), the branch's commits (the subject, author and age, with the first line of the description under them), its files as a tree with breadcrumbs and the directory's README rendered under it, a file's text (Markdown rendered, code as text, up to 1 MiB), and a commit's message, files changed and diff. In replay, commits after the timeline's position are hidden. With the firm off there is no current repository, and the tab offers to open the last run. The repository itself is `state/firms/<firm>/runs/<run>/git/product.git` under `~/.local/state/zapper`, a bare repository you can clone.

### Statistics

For the whole firm and per worker, over the whole run (up to the timeline's position in replay):

- **Activity**: a timeline per worker of idle, working, waiting and blocked, and the share of time in each.
- **Usage**: turns and tokens per worker and per role, against the budgets; rate-limit retries.
- **Mail**: messages sent and received per worker, assignments opened and closed, median time to reply, overdue requests, follow-ups and escalations.
- **Output**: artifacts and versions published per worker and kind.
- **Security**: requests allowed, denied and asked; decisions per officer; median time to decide; standing approvals added and revoked.

Charts are drawn as SVG built from DOM nodes, like the rest of the UI, with no chart library.

The board's writes (sending mail, firm up/down/pause, revoking or promoting standing approvals, answering escalations) have the same guards as answering approvals: token cookie, local `Host`, `X-Zapper-UI: 1`, local `Origin`.

### Avatars

Schematic, flat and calm, in the spirit of Claude's design: warm off-white background, clay orange (`#D97757`) as the accent, one muted color per role, round shapes, thin strokes, no faces beyond two dots, and a small spark as the "thinking" mark. Drawn as inline SVG from parts (head, body, role prop), animated with CSS only.

| Role | Prop |
|---|---|
| CEO | a slim tie and a small flag |
| Product manager | a sticky note on a small board |
| Manager | a clipboard |
| Engineer | a laptop with `</>` |
| Designer | a palette and a brush |
| Researcher | a magnifying glass over a page |
| Analyst | a bar chart |
| Security officer | a shield |

| State | Animation | When (from the journal) |
|---|---|---|
| Off | grey, outline only | no session for the worker |
| Booting | fading in | session started, Claude process not yet ready |
| Idle | slow breathing, an occasional blink | Claude ready, no turn running, nothing waiting on others |
| Waiting | small hourglass, slower breathing | no turn running, desk has threads waiting on others |
| Queued | standing, looking up | mail is ready for it but `max_concurrent` workers are already in a turn |
| Working | spark rotating above the head, prop animates (typing, bars rising, pages turning) | from the wake prompt to the `result` event |
| Reviewing | shield pulses, spark rotating | an officer's turn that holds an access request |
| Over budget | dimmed, a small gauge | the worker's or firm's budget is used up |
| Error | dimmed, red dot | the Claude process keeps failing, or the VM stopped |

Badges sit on top of any state: unread mail (a count), an access request pending (a padlock), overdue threads (a clock).

`prefers-reduced-motion` turns the animations into static state icons.

## State

Every `firm up` starts a fresh **run**: an idle firm with empty mailboxes, an empty shared drive and no standing approvals. A run is written to disk as it happens, and the daemon keeps only small indexes in memory (threads, desks, counters, offsets into the journal), so its memory stays low however long the run.

```
state/firms/<firm>/runs/<run-id>/
  run.json             start and end, the firm's config as booted (secrets redacted), worker → session ids
  journal.jsonl        every firm event: mail, follow-ups, state changes, access requests and decisions,
                       publishes, budgets; the source for the UI, statistics and replay
  transcripts/<worker>.jsonl   each worker's Claude stream-json events
  drive/<sha256>       shared drive files
```

- Each worker's session keeps its own audit log and terminal recordings in `state/sessions/<id>/`, as any zapper session does; `run.json` links them.
- Workers' disks are deleted when their VMs stop, as for any zapper session, so their Claude conversations and working files end with the run. What was published stays in the run's `drive/`.
- `zapper firm down` or a daemon restart ends the run. The next `firm up` starts a new one; it doesn't continue the old one.
- Runs stay on disk as records until removed: `zapper firm runs` lists them, `zapper firm prune <run>` deletes one. Like session directories, they hold sensitive material.

### Replay

The daemon can open a finished run read-only: `zapper firm replay <run>`, or the Runs menu in the UI. Nothing boots: no VMs, no Claude, no mail delivery. The UI shows the run as it was, with a timeline in the header: play and pause, speed (1×, 10×, 60×), jump to the next event, and drag to any moment. Avatars move through their states, envelopes fly, threads fill in, decisions appear, and transcripts and shared drive files open as they were at that moment. All writes are disabled. A replay can be opened while a firm is running.

A run can't be resumed ([limitations](#limitations)).

## Workers manipulating workers

Mail from one worker goes into another worker's prompt, and a worker's access request reason goes into an officer's prompt. Workers can try to persuade, mislead or instruct each other this way (prompt injection between agents). That is deliberately left open as an experiment: the prompts state who a message is from and don't defend further, and the run's journal and transcripts show what each worker tried and what worked.

What the host enforces regardless of what any prompt says:

- **Identity**: the sender is the VM the request came from. A body that says "the board says…" is text; only the header the post office writes says who sent it.
- **Authority**: assignments only down the chart, access decisions only by officers and the board, and the limits on officers.
- **Policy**: network, exec, secrets and `final` rules, as for any session.
- **Budgets and guards**: turns, tokens, loops, fan-out and sizes.

Things to watch for in runs: a worker talking an officer into a standing approval, a worker getting its manager to issue an assignment it wants, a worker quoting the board, and data leaving through an allowed host or the shared drive.

## Claude Code in the image

The `office` image (`make image IMAGE=office`) installs one Claude Code version, pinned in the Makefile (`CLAUDE_VERSION`) and installed with `install.sh` given that version, plus git, kubectl, podman, Node.js, Python, and a headless Chromium through Playwright (`PLAYWRIGHT_VERSION`). The daemon puts a `browse` command in every worker's VM (`browse shot URL file.png`, `browse text URL`, `browse console URL`); workers use it to see the product as users do, take screenshots for their reports, and, for officers, look at a site before deciding. Auto-update is off (`DISABLE_AUTOUPDATER=1` in the profile's environment), so a run uses exactly the version the image was built with, and the version is recorded in `run.json`. Updating is a change to the Makefile and an image rebuild.

## Prompts

The wake prompts, the follow-up and escalation texts, the officers' prompts, the office preamble in every worker's `CLAUDE.md`, and an example kickoff message from the board are in [office prompts](office-prompts.md).

## Testing

Real Claude makes tests slow, costly and different every time. Office tests use a **fake Claude** instead: `zapper-agent fake-claude`, which speaks Claude Code's stream-json on stdin and stdout and plays a script, calling tools through the real MCP server and office port. A role uses it with `fake: <script path>`:

```yaml
rules:                                        # the first rule that matches a wake runs
  - when: { from: board, kind: assignment }   # also: subject, contains, once: true
    do:
      - mail_send: { to: [ada], subject: "Write hello", body: "...", kind: assignment, due: 1h }
      - mail_reply: { message: $message, body: "Delegated to ada.", kind: reply }
  - when: { contains: "Access request r-" }
    do:
      - access_decide: { request: $request, allow: true, scope: worker, reason: "needed for the task" }
```

Actions are tool calls, `shell: CMD`, `say: TEXT` and `sleep: SECS`; `$message`, `$thread`, `$from`, `$subject` come from the wake's first message, `$request` from its first access request, `$artifact` from the last publish.

`make test-e2e-office` (part of `make test-e2e`) boots firms of real VMs and checks: skills, preamble and read-only mounts; delegation and reports; authority (a report can't assign its manager), identity (the sender is the VM) and mail privacy; officer-only tools; an `ask` refused at once, decided by an officer, and retried; the shared drive, including that symlinks aren't published; follow-ups; the office port outside a firm; runs, journals, transcripts and fresh restarts. With the k3s image and kernel built it also boots a cluster: kubeconfig and git for engineers, neither for others, the ingress for everyone, the product port, and cleanup at `firm down`.

A run with real Claude Code is a manual check: a two-worker firm on a small model (see [testing](testing.md)).

## Limitations

- **A run can't be resumed.** A daemon restart or `firm down` ends it; workers' disks and conversations go with it. The run stays on disk for replay. Resuming needs persistent worker disks and `--resume` of each conversation.
- **Mail waits for the end of a turn.** A worker deep in a long turn sees urgent mail only when the turn ends. A `Stop` hook could mention it sooner.
- **Officers decide network access only.** Exec rules in firm profiles are allow or deny, so a tool an officer lets a worker download can run unless a profile denies it.
- **The cluster is one node and starts fresh.** It holds nothing between runs; the git repository is the record of the product's code.
- **Inspection rules don't apply to the firm's own services.** Rules for `git.`, `cluster.` and `apps.` names match host and port; methods, paths and secrets on them are refused.
- **Workers share one subscription.** `max_concurrent` keeps turns within its rate limits; a refused turn is retried after a minute.

## References

- Claude Code headless mode and stream-json: https://code.claude.com/docs/en/headless
- Claude Code skills: https://code.claude.com/docs/en/skills
- Claude Code MCP servers: https://code.claude.com/docs/en/mcp
