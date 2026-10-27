#!/usr/bin/env bash
# Unpack a distro-signed package in the workspace; never install/start a service.
set -euo pipefail
project=$(cd "$(dirname "$0")/.." && pwd)
work="$project/.interop/xrdp"
mkdir -p "$work"
cd "$work"
if [ ! -x root/usr/sbin/xrdp ]; then
    apt-get download xrdp
    for package in xrdp_*.deb; do dpkg-deb -x "$package" root; done
fi
export LD_LIBRARY_PATH="$work/root/usr/lib/x86_64-linux-gnu/xrdp:$work/root/usr/lib/xrdp${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
root/usr/sbin/xrdp --help
find root -name '*.fv1'
ls root/etc/xrdp
