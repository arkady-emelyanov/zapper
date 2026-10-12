#!/bin/sh
# Downloads Firecracker (checksum-verified) and a guest kernel into
# ~/.local/share/zapper. Re-running skips what is already there.
#
#   scripts/fetch-deps.sh                 # Firecracker v1.17.0, kernel 6.1
#   FC_VERSION=v1.17.0 scripts/fetch-deps.sh
set -eu

FC_VERSION=${FC_VERSION:-v1.17.0}
KERNEL_URL=${KERNEL_URL:-https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.15/x86_64/vmlinux-6.1.155}
KERNEL_NAME=${KERNEL_NAME:-vmlinux-6.1}
ARCH=$(uname -m)
DATA=${ZAPPER_DATA:-$HOME/.local/share/zapper}

mkdir -p "$DATA/bin" "$DATA/kernels" "$DATA/images"

if [ -x "$DATA/bin/firecracker" ] && "$DATA/bin/firecracker" --version | grep -q "${FC_VERSION#v}"; then
    echo "firecracker $FC_VERSION: present"
else
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    url=https://github.com/firecracker-microvm/firecracker/releases/download/$FC_VERSION/firecracker-$FC_VERSION-$ARCH.tgz
    echo "downloading $url"
    curl -fsSL -o "$tmp/fc.tgz" "$url"
    tar -C "$tmp" -xzf "$tmp/fc.tgz"
    dir=$tmp/release-$FC_VERSION-$ARCH
    (cd "$dir" && grep -E " \./(firecracker|jailer)-$FC_VERSION-$ARCH\$" SHA256SUMS | sha256sum -c -)
    install -m 0755 "$dir/firecracker-$FC_VERSION-$ARCH" "$DATA/bin/firecracker"
    install -m 0755 "$dir/jailer-$FC_VERSION-$ARCH" "$DATA/bin/jailer"
    echo "firecracker $FC_VERSION: installed in $DATA/bin"
fi

if [ -s "$DATA/kernels/$KERNEL_NAME" ]; then
    echo "kernel $KERNEL_NAME: present"
else
    echo "downloading $KERNEL_URL"
    curl -fsSL -o "$DATA/kernels/$KERNEL_NAME.tmp" "$KERNEL_URL"
    curl -fsSL -o "$DATA/kernels/$KERNEL_NAME.config" "$KERNEL_URL.config" || true
    mv "$DATA/kernels/$KERNEL_NAME.tmp" "$DATA/kernels/$KERNEL_NAME"
    echo "kernel $KERNEL_NAME: installed in $DATA/kernels"
fi
