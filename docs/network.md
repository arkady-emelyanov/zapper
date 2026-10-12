[← README](../README.md) · [Docs](README.md) · **Network**

# Network

## Overview

A session reaches the network in one of two modes, set per profile with `engine.network`:

| Mode | The guest has | Which programs get out | Host needs |
|---|---|---|---|
| `nic` (default) | `eth0` with an address, a default route and a DNS server | Any program, over TCP | `ip` and `nft` |
| `vsock` | Loopback, and `HTTP(S)_PROXY` set to a local forwarder | Programs that use the proxy variables | Nothing beyond what every session needs |

Every session needs unprivileged user namespaces on the host, because the VMM runs confined in its own namespaces ([security](security.md#the-vmm)). In vsock mode the VMM's network namespace is empty.

In both modes, each connection is decided by the session's `net` rules on the host ([policy](policy.md)). Allowed connections are made by the host.

## How it works

### NIC mode

The daemon starts the VMM through `zapper __vmm`. This shim:

1. enters a new user namespace and network namespace (and a mount namespace, for [confinement](security.md#the-vmm)), and becomes root inside them without any host privileges;
2. creates `tap0` with `10.0.2.1/30`, plus loopback. These are the namespace's only interfaces, and it has no routes to the host's network;
3. loads nftables rules that redirect every TCP connection arriving on `tap0` to a local listener, and every UDP packet to port 53 to a local DNS listener, and drop the remaining traffic;
4. passes each captured connection to the daemon as `TCP <destination> <port>`, and each query as `DNS`, over `nic.sock` in the session's VMM directory;
5. starts Firecracker, which attaches `tap0` to the guest as `eth0`.

The guest kernel configures `eth0` as `10.0.2.2/30`, with gateway and DNS server `10.0.2.1`. The capture runs in the namespace, outside the VM, so changing routes or DNS settings inside the guest, even as root, still leaves all traffic going through it.

### DNS

The daemon answers the guest's DNS queries. For each name it decides whether some request to it could be allowed:

- A `final` deny that has no `ports`, `methods` or `path` and matches the name means no.
- Any `allow` or `ask` rule whose `host` matches the name means yes.
- Otherwise the default `net` action decides.

An allowed name gets an address from `198.18.0.0/15`, unique to that name within the session. Other names get NXDOMAIN. AAAA queries for allowed names get an empty answer, so clients use IPv4. The daemon doesn't contact any DNS server while answering, so the guest can't use queries to send data out. Each A query and each refusal is a `dns` event.

### Connections

When the guest connects to one of these synthetic addresses, the daemon knows which name it stands for and decides on that name and the port. This works for any TCP protocol, such as HTTPS, SSH, git or a database. A connection to any other address is decided on the address itself.

For an allowed connection, the daemon:

1. resolves the name on the host and checks the address ([address checks](#address-checks));
2. connects to the address it checked;
3. if the client starts with a TLS ClientHello, requires its server name to equal the decided name;
4. relays the bytes.

If a rule for the host has `methods`, `path`, `secrets` or `set_headers`, the connection is inspected instead. For TLS, the daemon completes the handshake with a certificate from the session's CA, which the guest trusts. It then decides each request on its method and path, sets `Host` to the decided name, adds secrets, and forwards the request over a verified TLS connection. Plain HTTP is inspected the same way, without the TLS step.

### vsock mode

The guest has loopback only. `HTTP_PROXY` and `HTTPS_PROXY` point at `127.0.0.1:3128`, which the agent forwards over vsock to the daemon. The daemon serves the HTTP proxy protocol: a `CONNECT host:port` is handled like a captured connection to that name, and an absolute `http://` request is decided and forwarded on its own.

### Address checks

The daemon connects only to addresses that pass these checks, whatever the rules say:

| Address | Reachable |
|---|---|
| Loopback, link-local (including cloud metadata `169.254.169.254`), multicast, unspecified | Never |
| The host's own addresses | Never |
| Private and unique-local ranges (`10/8`, `172.16/12`, `192.168/16`, `100.64/10`, `198.18/15`, `fc00::/7`) | If listed in `daemon.private_networks` |
| Public addresses | Yes |

## Usage

Rules name hosts and ports. These are examples for a profile's `policy.net`:

```
- { action: allow, host: github.com, ports: [22] }                     # git over SSH
- { action: allow, host: "*.githubusercontent.com", ports: [443] }     # raw files, passed through
- { action: allow, host: pypi.org, ports: [443], methods: [GET] }      # inspected: GET only
- { action: allow, host: db.lab.internal, ports: [5432] }              # needs the lab range in private_networks
- { action: ask, host: "*" }                                            # anything else waits for approval
```

To see what a session does and why:

```
zapper events -s ID                          # dns and net events, with the deciding rule
zapper eval -s ID net github.com:22          # which rule would decide, before changing anything
```

From inside a guest, a denied name fails to resolve (`getent hosts` prints nothing), and a denied connection is closed as soon as it's made. An inspected HTTP request that is denied gets a `403` with the rule's id.

## Configuration

| Key | Where | Meaning |
|---|---|---|
| `engine.network` | profile | `nic` or `vsock` |
| `daemon.private_networks` | daemon | CIDRs of private networks sessions may reach |
| `net` rules | any policy layer | [policy](policy.md) |

## Verification

```
zapper check                  # "unprivileged user and mount namespaces", "tap and nftables in the VMM's network namespace"
make test-e2e                 # the NIC, root and vsock groups in docs/testing.md
```

## Limitations

- TCP and DNS are the only traffic that leaves a NIC session. QUIC clients fall back to TCP. IPv6 isn't routed.
- A program that connects to an address without looking up a name is decided on that address. Its TLS server name then has to equal the address.
- A server-first protocol such as SSH waits up to 300 ms for its first bytes, while the daemon checks whether the client is starting TLS.
- A NIC adds about 0.4 s to session start, for the namespace and its nftables rules.

## References

- Sources: `crates/zapper/src/netns.rs`, `crates/zapper/src/daemon/{nic,dns,proxy,addr}.rs`, `crates/zapper-agent/src/forward.rs`
