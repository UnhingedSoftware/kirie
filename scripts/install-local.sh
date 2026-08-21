#!/bin/bash
# Build kirie with the webview backend, install to ~/.local/bin, restart the
# running engine. The one supported way to update a live setup from the repo:
# installing a binary built without a web feature silently drops every web
# wallpaper from pickers and renders the current one black, so this refuses
# to install one.
set -euo pipefail
cd "$(dirname "$0")/.."

# One binary: the webview host is compiled into the engine and reached by
# re-executing it (`kirie __webviewhost`), which also retires the stale-sibling
# footgun — a `kirie-webviewhost` left in ~/.local/bin by an older install used
# to shadow every webview fix until someone noticed it was two days old.
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
