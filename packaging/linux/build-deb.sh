#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "Usage: $0 <release-binary> <version> <output-dir>" >&2
  exit 2
fi

binary=$(realpath "$1")
version=$2
output=$(realpath -m "$3")
repo_root=$(cd "$(dirname "$0")/../.." && pwd)
root=$(mktemp -d)
debroot="$root/deb"
trap 'rm -rf "$root"' EXIT

mkdir -p "$output" \
  "$debroot/DEBIAN" \
  "$debroot/usr/bin" \
  "$debroot/usr/share/applications" \
  "$debroot/usr/share/doc/rumstsc"

install -m 0755 "$binary" "$debroot/usr/bin/rumstsc"
install -m 0644 "$repo_root/assets/rumstsc.desktop" "$debroot/usr/share/applications/rumstsc.desktop"
install -Dm 0644 "$repo_root/assets/rumstsc.svg" "$debroot/usr/share/icons/hicolor/scalable/apps/rumstsc.svg"
for size in 16 24 32 48 64 128 256 512; do
  install -Dm 0644 "$repo_root/assets/icons/${size}x${size}/rumstsc.png" "$debroot/usr/share/icons/hicolor/${size}x${size}/apps/rumstsc.png"
done
install -m 0644 "$repo_root/LICENSE" "$debroot/usr/share/doc/rumstsc/copyright"
install -m 0644 "$repo_root/README.md" "$debroot/usr/share/doc/rumstsc/README.md"

cat > "$debroot/DEBIAN/control" <<EOF
Package: rumstsc
Version: $version
Section: net
Priority: optional
Architecture: amd64
Maintainer: Cattens-Roses <https://github.com/Cattens-Roses/rumstsc>
Depends: libfontconfig1, libfreetype6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libxcb1, libx11-xcb1, libssl3, libvulkan1, libavcodec60 | libavcodec61 | libavcodec58, libavutil58 | libavutil59 | libavutil56, libswscale7 | libswscale8 | libswscale5
Description: Minimal experimental RDP client
 Native Rust + GPUI remote desktop client with an independent RDP engine.
EOF

cat > "$debroot/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
fi
exit 0
EOF
chmod 0755 "$debroot/DEBIAN/postinst"

dpkg-deb --build --root-owner-group "$debroot" "$output/rumstsc_${version}_amd64.deb"

echo "Created package:"
ls -lh "$output/rumstsc_${version}_amd64.deb"
