#!/usr/bin/env bash
# Local interoperability smoke test. Requires scripts/prepare-xrdp.sh first.
set -euo pipefail
project=$(cd "$(dirname "$0")/.." && pwd)
work="$project/.interop/xrdp"
if [ "${1:-}" != --inside ]; then
    if [ "$(id -u)" != 0 ]; then
        echo 'Run this isolated mount-namespace test as root (or through sudo).' >&2
        exit 1
    fi
    # Create TLS material on the host filesystem before entering the mount
    # namespace. Overlaying /etc with tmpfs hides /etc/ssl/openssl.cnf, and
    # openssl req then fails with no useful diagnostics under set -e.
    if [ ! -f "$work/key.pem" ] || [ ! -f "$work/cert.pem" ]; then
        openssl req -x509 -newkey rsa:2048 -nodes \
            -keyout "$work/key.pem" -out "$work/cert.pem" -days 1 \
            -subj /CN=localhost \
            -addext subjectAltName=DNS:localhost,IP:127.0.0.1
    fi
    openssl x509 -in "$work/cert.pem" -outform DER \
        | sha256sum | cut -d' ' -f1 > "$work/pin.txt"
    script=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
    exec unshare --mount --propagation private bash "$script" --inside "${1:-tls}"
fi
mode=${2:-tls}
case "$mode" in tls) layer=tls;; legacy) layer=rdp;; *) echo 'mode must be tls or legacy' >&2; exit 1;; esac
# The package embeds /etc/xrdp and /usr/share/xrdp. Supply these only in
# a private mount namespace; the host filesystem is never modified.
if [ "$(readlink /proc/self/ns/mnt)" = "$(readlink /proc/1/ns/mnt)" ]; then
    echo 'Refusing resource mounts in the host mount namespace' >&2
    exit 1
fi
pin=$(cat "$work/pin.txt")
mount -t tmpfs tmpfs /etc
mkdir -p /etc/xrdp
cp "$work/root/etc/xrdp/"*ini /etc/xrdp/
mount -t tmpfs tmpfs /usr/share
mkdir -p /usr/share/xrdp
cp -r "$work/root/usr/share/xrdp/". /usr/share/xrdp/
mount -t tmpfs tmpfs /run
mkdir -p /run/xrdp/sockdir
export LD_LIBRARY_PATH="$work/root/usr/lib/x86_64-linux-gnu/xrdp:$work/root/usr/lib/xrdp${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
"$work/root/usr/bin/xrdp-keygen" xrdp /etc/xrdp/rsakeys.ini >/dev/null

cat > "$work/smoke.ini" <<EOF
[Globals]
ini_version=1
fork=false
port=tcp://127.0.0.1:33991
security_layer=$layer
crypt_level=high
certificate=$work/cert.pem
key_file=$work/key.pem
ssl_protocols=TLSv1.2,TLSv1.3
allow_channels=false
allow_multimon=false
bitmap_cache=true
bitmap_compression=true
bulk_compression=${RUMSTSC_TEST_BULK:-true}
max_bpp=24
use_fastpath=none
fv1_select=*:sans-10.fv1
ls_title=Isolated rumstsc test
ls_logo_filename=$work/root/usr/share/xrdp/xrdp_logo.bmp
[Logging]
LogFile=$work/server.log
LogLevel=DEBUG
EnableSyslog=false
EnableConsole=true
ConsoleLevel=INFO
[Xorg]
name=Xorg
lib=libxup.so
username=ask
password=ask
ip=127.0.0.1
port=-1
EOF
"$work/root/usr/sbin/xrdp" --nodaemon --config "$work/smoke.ini" > "$work/console.log" 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true; sleep 1; kill -9 "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true' EXIT
sleep 1
kill -0 "$server"
cd "$project"
probe="$project/.interop/target-linux/debug/rumstsc-probe"
if [ "$mode" = legacy ]; then
    # This is an isolated local server; bootstrap its test pin before sending
    # client information. Never do this automatically for an untrusted host.
    pin=$("$probe" --mode legacy 127.0.0.1:33991 | sed -n 's/^Legacy certificate SHA-256 //p')
fi
"$probe" --mode "$mode" --pin "$pin" --session --trace "$work/trace-$mode.json" --output "$work/frame-$mode.ppm" 127.0.0.1:33991
