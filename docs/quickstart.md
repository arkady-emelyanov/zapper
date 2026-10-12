[← README](../README.md) · [Docs](README.md) · **Quickstart**

# Quickstart

From an empty machine to your first sandboxed session, then a rule, then Claude Code. About 15 minutes, most of it downloads.

## 1. What you need

- Linux on x86_64, with KVM (`ls /dev/kvm`)
- Rust, with the static target: `rustup target add x86_64-unknown-linux-musl`
- `podman` and `e2fsprogs`, to build images
- `iproute2` and `nftables`
- unprivileged user namespaces (on by default on most distributions)

`zapper check` in step 4 tells you if something is missing, with the command that fixes it.

## 2. Build and install

```
git clone <this repository> && cd zapper
make deps         # downloads Firecracker and a guest kernel (~1 min)
make install      # builds zapper and puts it in ~/.local/bin (~2 min)
```

## 3. Create the config and an image

```
zapper init       # writes ~/.config/zapper/zapper.yaml
make image        # builds the Ubuntu image the default profile uses (~2 min)
```

The config comes with a few profiles. A profile is a recipe for a session: which image, which command, which rules. `zapper profile ls` lists them once the daemon runs.

## 4. Check

```
zapper check
```

Each line should say `ok`. Two things are expected at this point:

- `ERROR  profile study-python: image python`: that profile needs an image you haven't built. Ignore it, or run `make image IMAGE=python`.
- `note   the daemon isn't running`: you start it next.

## 5. Start the daemon

In a terminal of its own:

```
zapper serve
```

```
zapper: UI at http://127.0.0.1:7878/?token=3evtji2k5mntwur2ijmc5xm2
zapper: API on /run/user/1000/zapper/api.sock
```

Open that URL to watch sessions in your browser and answer approvals. Leave the daemon running.

## 6. Your first session

In another terminal, go to any project folder and run:

```
cd ~/some/project
zapper run
```

You get a shell inside a VM, in `/work`, which holds a copy of your folder. Try:

```
ls                                  # your files
curl https://pypi.org               # fails: the default profile allows no network
exit
```

The session and its VM are removed when you exit. Your folder is unchanged.

## 7. Allow something

```
zapper rule add profile:default '{kind: net, action: allow, host: pypi.org, ports: [443], methods: [GET]}'
zapper run -- curl -sS -o /dev/null -w "%{http_code}\n" https://pypi.org/simple/requests/
```

```
200
```

The rule applies at once. It isn't saved yet:

```
zapper config diff      # shows the new rule
zapper config save      # writes it to ~/.config/zapper/zapper.yaml
```

## 8. See what happened

```
zapper events --history
```

```
07:37:13 zdcpn8 dns     allow pypi.org → 198.18.0.1
07:37:14 zdcpn8 net     allow GET pypi.org:443/simple/requests/ → 200  [net-7]
```

Each line is one thing the session did, and the rule that allowed or denied it.

## 9. Get results out

The VM works on a copy, so copy results back explicitly:

```
zapper run --keep                    # prints the session id when you exit, e.g. zdcpn8
zapper cp zdcpn8:/work ./results
zapper rm zdcpn8
```

## 10. Claude Code

```
make image IMAGE=claude              # an image with Claude Code (~3 min)
claude setup-token                   # on the host: prints a long-lived token
```

Save the token where the config expects it, readable only by you:

```
install -m 600 /dev/null ~/.config/zapper/claude-token
$EDITOR ~/.config/zapper/claude-token      # paste the token
```

Then:

```
zapper run -p claude
```

Claude Code starts inside the VM. The token stays on the host: zapper adds it to requests to `api.anthropic.com`, and Claude only sees a placeholder.

Usage counts against your subscription, not API billing. Claude Code's banner says "Claude API" anyway, because this kind of token can't read your plan; `/status` shows "Auth token: CLAUDE_CODE_OAUTH_TOKEN", and `/usage` shows your plan's limits. The comments on `secrets` in the config explain the difference, and how to switch to an API key.

## Next

- [Policy](policy.md): writing rules, secrets, approvals.
- [Configuration](configuration.md): profiles and the config file.
- [Commands](commands.md): every command.
