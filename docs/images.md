[← README](../README.md) · [Docs](README.md) · **Images and kernels**

# Images and kernels

## Overview

An image is an ext4 file built from a container image's filesystem. A profile selects one with `engine.image`, and each session boots its own copy. A kernel is an uncompressed `vmlinux`, selected with `engine.kernel`. The guest agent comes from an initramfs that the daemon builds when it starts, so images don't need it.

## How it works

`zapper image build NAME --from REF [--run CMD]…`:

1. Pulls `REF` with podman, or builds `FROM REF` with one `RUN` per `--run`. `$ZAPPER_OCI` selects another engine.
2. Exports the container's filesystem as a tar.
3. Extracts the tar and runs `mkfs.ext4 -d` inside `podman unshare`, the engine's user namespace. This keeps the image's file ownership without needing root; outside it, every file would belong to the building user, and uid 1000 in the guest would own `/usr`.
4. Writes `images/NAME.ext4`, and `images/NAME.json` with the source, steps, size and build time.

The image is sized at its content plus 25% plus 256 MiB, unless `--size` is given. Each session's copy is grown to `engine.disk`.

## Usage

```
make image                                     # ubuntu-24.04 with curl, git and CA certificates (~2 min)
make image IMAGE=claude                        # the same, plus ripgrep and Claude Code (~3 min)
make image IMAGE=python                        # python:3.13-slim with curl and git
zapper image build rust --from docker.io/library/rust:1-slim --run "apt-get update && apt-get install -y git"
zapper image ls
```

For TLS through inspected connections, an image needs a CA bundle at `/etc/ssl/certs/ca-certificates.crt`, as the `ca-certificates` package provides. The agent adds the session CA to it at boot.

`make deps` downloads the Firecracker CI kernel 6.1 to `kernels/vmlinux-6.1`, with its config alongside. Another kernel works if it has these built in: virtio-mmio block and net, vsock, `IP_PNP`, devtmpfs, ext4, seccomp filters, cgroup2 and BSD process accounting (`CONFIG_BSD_PROCESS_ACCT`).

## Verification

```
zapper image ls                                # name, size and source of each image
zapper run -p default -- cat /etc/os-release   # boots the image
```

## Limitations

- Sessions change their own copy, and the image stays as built. Put tools in the image, or install them with the profile's `setup` steps.
- Image builds run with full network access, as trusted operator steps.

## References

- Firecracker CI kernels: https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md
- Sources: `crates/zapper/src/image.rs`, `crates/zapper/src/daemon/vm.rs`, `scripts/fetch-deps.sh`
