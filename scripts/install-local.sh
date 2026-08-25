#!/bin/bash
# Build kirie with the webview backend, install to ~/.local/bin, restart the
# running engine. The one supported way to update a live setup from the repo:
# installing a binary built without a web feature silently drops every web
# wallpaper from pickers and renders the current one black, so this refuses
# to install one.
set -euo pipefail
cd "$(dirname "$0")/.."

# One installed file, but the host is NOT linked into the engine: gtk in the
# engine's NEEDED maps the whole gtk/gdk/cairo/pango chain at exec for every
# wallpaper, scene-only ones included (measured: +10 MB peak, +267 mappings).
# Build the host first, then embed it; the engine extracts it to the cache on
# first web use, keyed by content hash so a rebuilt host is never shadowed by a
# stale one.
cargo build --release -p kirie-web --features webview --bin kirie-webviewhost
KIRIE_EMBED_WEBVIEWHOST="$PWD/target/release/kirie-webviewhost" \
    cargo build --release -p kirie --features web-webview,vaapi

if ! ./target/release/kirie check 2>/dev/null | grep -q "web backend (webview)"; then
    echo "refusing to install: built kirie has no web backend" >&2
    exit 1
fi

install -m 755 target/release/kirie "$HOME/.local/bin/kirie"
# An older split install leaves a sibling host that is now dead weight; the
# engine ignores it, but keeping it around invites confusion.
rm -f "$HOME/.local/bin/kirie-webviewhost"

restart="$HOME/.config/hypr/wallpaper-daemon/wallpaperengine-restart.sh"
[ -x "$restart" ] && "$restart"
echo "installed + restarted"
