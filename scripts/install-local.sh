#!/bin/bash
# Build kirie with the webview backend, install to ~/.local/bin, restart the
# running engine. The one supported way to update a live setup from the repo:
# installing a binary built without a web feature silently drops every web
# wallpaper from pickers and renders the current one black, so this refuses
# to install one.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p kirie -p kirie-web --features kirie/web-webview,kirie/vaapi

if ! ./target/release/kirie check 2>/dev/null | grep -q "web backend (webview)"; then
    echo "refusing to install: built kirie has no web backend" >&2
    exit 1
fi

install -m 755 target/release/kirie "$HOME/.local/bin/kirie"
install -m 755 target/release/kirie-webviewhost "$HOME/.local/bin/kirie-webviewhost"

restart="$HOME/.config/hypr/wallpaper-daemon/wallpaperengine-restart.sh"
[ -x "$restart" ] && "$restart"
echo "installed + restarted"
