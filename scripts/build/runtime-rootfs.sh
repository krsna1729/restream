#!/usr/bin/env bash
# Assemble the smallest root filesystem needed by a native-linked Restream
# binary. This is intentionally reusable outside Docker so the scratch runtime
# contract has one owner rather than a Dockerfile-only copy of `ldd` output.
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <restream-binary> <rootfs-dir>" >&2
    exit 2
fi

binary=$1
rootfs=$2

if [[ ! -x "$binary" ]]; then
    echo "runtime-rootfs: executable not found: $binary" >&2
    exit 1
fi

restream_home="$rootfs/.restream"
mkdir -p \
    "$restream_home" \
    "$restream_home/runtime" \
    "$restream_home/data" \
    "$restream_home/media" \
    "$restream_home/logs" \
    "$rootfs/etc/ssl/certs" \
    "$rootfs/usr/share/zoneinfo"

cp -a /usr/share/zoneinfo/. "$rootfs/usr/share/zoneinfo/"
cp -a /etc/localtime "$rootfs/etc/localtime"
cp /etc/ssl/certs/ca-certificates.crt "$rootfs/etc/ssl/certs/ca-certificates.crt"
cp /etc/nsswitch.conf "$rootfs/etc/nsswitch.conf"
cp /etc/protocols "$rootfs/etc/protocols"
cp /etc/services "$rootfs/etc/services"
cp -L /etc/resolv.conf "$rootfs/etc/resolv.conf"
printf 'restream:x:1000:1000:restream:/nonexistent:/sbin/nologin\n' > "$rootfs/etc/passwd"
printf 'restream:x:1000:\n' > "$rootfs/etc/group"
if [[ "$(id -u)" -eq 0 ]]; then
    chown -R 1000:1000 "$restream_home"
else
    # Docker builds this rootfs as root, so the scratch image still gets a
    # writable /.restream for USER 1000:1000. Release tarball packaging runs as
    # an unprivileged CI user and only uses rootfs/ as a loader/library closure;
    # `./run restream` creates .restream beside the bundle, not inside rootfs/.
    echo "runtime-rootfs: skipping .restream chown; not running as root"
fi

# Keep this parser deliberately constrained to absolute ELF paths reported by
# ldd. Any failure aborts the package step: a scratch image without its loader
# is worse than a larger image because it fails only at operator startup.
ldd "$binary" | awk '$3 ~ /^\// { print $3 } $1 ~ /^\// { print $1 }' \
    | sort -u \
    | while IFS= read -r library; do
        cp --parents -L "$library" "$rootfs"
    done

test -e "$rootfs/lib64/ld-linux-x86-64.so.2"
echo "runtime-rootfs: assembled $rootfs for $binary"
