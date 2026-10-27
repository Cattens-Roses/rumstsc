#!/usr/bin/env bash
set -euo pipefail
project=$(cd "$(dirname "$0")/.." && pwd)
for pid in $(pgrep -x rumstsc-probe || true); do
    if [ "$(readlink "/proc/$pid/exe")" = "$project/.interop/target-linux/debug/rumstsc-probe" ]; then
        kill "$pid"
    fi
done
