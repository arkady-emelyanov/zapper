[← README](../README.md) · [Docs](README.md) · **Quickstart**

# Quickstart

From a fresh clone to a running firm: the example firm `acme`, with seven workers and a cluster to deploy to. About 30 minutes, most of it downloads and the cluster's kernel build.

## 1. What you need

- Linux on x86_64, with KVM (`ls /dev/kvm`)
- Rust, with the static target: `rustup target add x86_64-unknown-linux-musl`
- `podman` and `e2fsprogs`, to build images
- `iproute2` and `nftables`
- unprivileged user namespaces (on by default on most distributions)
- a Claude subscription, for the workers
- about 40 GiB of free RAM for the example firm ([office: resources](office.md#resources))

`zapper check` in step 5 tells you if something is missing, with the command that fixes it.

## 2. Build and install

```
git clone <this repository> && cd zapper
make deps         # downloads Firecracker and a guest kernel (~1 min)
make install      # builds zapper and puts it in ~/.local/bin (~2 min)
```

zapper is the daemon and CLI the office runs on. Run it from the repository's root: it uses `config/zapper.yaml` there, and paths in it are relative to that file or under your home. Outside a checkout, `zapper init` copies it to `~/.config/zapper/zapper.yaml`. The config defines the firm `acme` and the profiles its workers run with; a profile is a recipe for a VM: which image, which command, which rules.

## 3. Images and the cluster's kernel

```
make image                    # the base Ubuntu image (~2 min)
make image IMAGE=office       # the workers: Claude Code (pinned), git, kubectl, podman (~5 min)
make image IMAGE=k3s          # the firm's cluster (~2 min)
make kernel KERNEL=k8s        # the cluster's kernel, built in a container (~10 min)
```

## 4. The Claude token

Workers use your Claude subscription through a long-lived token:

```
claude setup-token                         # on the host: prints a long-lived token
install -m 600 /dev/null ~/.config/zapper/claude-token
$EDITOR ~/.config/zapper/claude-token      # paste the token
```

The token stays on the host: zapper adds it to the workers' requests to `api.anthropic.com`, and Claude Code in the VMs only sees a placeholder.

Usage counts against your subscription, not API billing. Claude Code's banner says "Claude API" anyway, because this kind of token can't read your plan; `/status` shows "Auth token: CLAUDE_CODE_OAUTH_TOKEN", and `/usage` shows your plan's limits. The comments on `secrets` in the config explain the difference, and how to switch to an API key.

## 5. Check

```
zapper check
```

Each line should say `ok`. Expected at this point:

- `note   the daemon isn't running`: you start it next.

## 6. Start the daemon

In a terminal of its own:

```
zapper serve
```

```
zapper: UI at http://127.0.0.1:7878/?token=3evtji2k5mntwur2ijmc5xm2
zapper: API on /run/user/1000/zapper/api.sock
```

Open that URL: the UI shows the firm, still off. Leave the daemon running.

## 7. Check a session

Each worker is a zapper session. Before booting a firm, check that a plain one works:

```
zapper run -C $(mktemp -d)
```

You get a shell inside a VM, in `/work`, which holds a copy of the directory given with `-C` (by default, the current one). Try:

```
curl https://pypi.org               # fails: the default profile allows no network
exit
```

The session and its VM are removed when you exit. `zapper events --history` shows what it did, one line per DNS query, connection or exec, with the rule that decided it.

## 8. Start the firm

```
zapper firm up acme                     # boots the cluster, then seven idle workers
zapper firm status acme                 # workers' states, mail, turns and tokens today
```

`firm up` checks the firm's memory against the host's before booting anything. In the UI, the org chart fills in as workers boot, and the product port, `http://127.0.0.1:8080`, shows the firm's landing page.

## 9. Give the CEO a task

Click the CEO in the UI and write the firm's first task, or:

```
zapper mail send acme --to barbara --kind assignment --subject "First product" --body "…"
```

Nothing in the config says what the firm builds; the kickoff does. [Office prompts](office-prompts.md#example-kickoff) has an example.

## 10. Watch it work

```
zapper firm journal acme -f             # mail, states, turns, access requests, publishes
zapper firm transcript acme ada         # what a worker's Claude did
zapper firm security acme               # access requests and standing approvals
zapper drive ls acme                    # what workers published
```

In the UI: envelopes move along the org chart as workers mail each other, the Messages tab holds every thread, Security shows the officers' decisions and anything escalated to you, and the Shared drive tab opens what was published. The product the engineers deploy replaces the landing page at `http://127.0.0.1:8080`, and the cluster's Kubernetes dashboard is at `http://127.0.0.1:8081` (the UI has its sign-in token).

`budget` in the config limits the turns per day; `zapper firm status acme` shows what's used.

## 11. End the run

```
zapper firm down acme
```

The run stays on disk: `zapper firm runs acme` lists runs, and the UI's Runs tab replays one. The next `zapper firm up acme` starts a fresh run.

## Next

- [Office](office.md): firms, mail, the shared drive, security officers, the UI.
- [Policy](policy.md): writing rules, secrets, approvals.
- [Configuration](configuration.md): profiles, firms and the config file.
- [Commands](commands.md): every command.
