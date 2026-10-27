#!/usr/bin/env bash
# Run through xvfb-run; capture both palettes without touching user preferences.
set -euo pipefail
project=$(cd "$(dirname "$0")/.." && pwd)
work="$project/.interop/gui"
mkdir -p "$work/config/rumstsc"
app_pid=
trap 'if [ -n "$app_pid" ]; then kill "$app_pid" 2>/dev/null || true; fi' EXIT
for theme in dark light; do
    printf 'version=1\ntheme=%s\nresolution=0\n' "$theme" > "$work/config/rumstsc/settings"
    env -u WAYLAND_DISPLAY XDG_CONFIG_HOME="$work/config" "$project/.interop/target-linux/debug/rumstsc" > "$work/$theme.log" 2>&1 &
    app_pid=$!
    window=
    for attempt in $(seq 1 30); do
        window=$(xdotool search --onlyvisible --name 'rumstsc' 2>/dev/null | head -1 || true)
        if [ -n "$window" ]; then break; fi
        kill -0 "$app_pid"
        sleep 0.2
    done
    test -n "$window"
    xdotool windowfocus "$window"
    sleep 8
    import -window "$window" "$work/$theme.png"
    if [ "$theme" = dark ]; then
        xdotool mousemove --window "$window" 180 420 click 1
        xdotool type --clearmodifiers 'temporary-test-password'
        sleep 0.5
        import -window "$window" "$work/password-masked.png"
    fi
    kill "$app_pid"
    wait "$app_pid" 2>/dev/null || true
    app_pid=
done
