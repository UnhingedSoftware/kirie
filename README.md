# kirie (切り絵)

A fast, memory-safe wallpaper renderer for Linux, compatible with
[Wallpaper Engine](https://www.wallpaperengine.io/) content. Written in Rust on
wgpu/Vulkan, fully multithreaded, with a hash-keyed prebaked scene-bundle cache.

Every wallpaper is validated scene by scene against how Wallpaper Engine itself
renders it, so what you see on Linux is what the author published.

Renders scene, video, image, and web wallpapers, with audio-reactive visualizers,
SceneScript (JS) scripting, 3D puppet/model layers, and the full effect pipeline.

## Gallery

Captured with `kirie --screenshot`, so these are kirie's own output rather than
the Workshop preview art. Titles link to the item on Steam.

| | |
|:-:|:-:|
| [![Tree of life](https://unhingedsoftware.github.io/kirie/shots/1388331347.jpg)](https://steamcommunity.com/sharedfiles/filedetails/?id=1388331347)<br>[Tree of life](https://steamcommunity.com/sharedfiles/filedetails/?id=1388331347) | [![Moonlight](https://unhingedsoftware.github.io/kirie/shots/3293156956.jpg)](https://steamcommunity.com/sharedfiles/filedetails/?id=3293156956)<br>[Moonlight](https://steamcommunity.com/sharedfiles/filedetails/?id=3293156956) |
| [![Cyberpunk: Edgerunners](https://unhingedsoftware.github.io/kirie/shots/3421423611.jpg)](https://steamcommunity.com/sharedfiles/filedetails/?id=3421423611)<br>[Cyberpunk: Edgerunners](https://steamcommunity.com/sharedfiles/filedetails/?id=3421423611) | [![Miku Monitoring](https://unhingedsoftware.github.io/kirie/shots/3585875739.jpg)](https://steamcommunity.com/sharedfiles/filedetails/?id=3585875739)<br>[Miku Monitoring](https://steamcommunity.com/sharedfiles/filedetails/?id=3585875739) |
| [![Ethereal Light Pillar](https://unhingedsoftware.github.io/kirie/shots/3648988375.jpg)](https://steamcommunity.com/sharedfiles/filedetails/?id=3648988375)<br>[Ethereal Light Pillar](https://steamcommunity.com/sharedfiles/filedetails/?id=3648988375) | [![冷冰冰的誓言](https://unhingedsoftware.github.io/kirie/shots/3600453929.jpg)](https://steamcommunity.com/sharedfiles/filedetails/?id=3600453929)<br>[冷冰冰的誓言](https://steamcommunity.com/sharedfiles/filedetails/?id=3600453929) (video) |

More, with per-item notes, on the
[compatibility list](https://unhingedsoftware.github.io/kirie/) — and
`kirie list` reports what is installed locally, including whether this build can
render each item.

**Add a wallpaper to the list**: [report it in an
issue](https://github.com/UnhingedSoftware/kirie/issues/new?template=wallpaper-report.yml)
(no repo checkout needed), or edit
[`site/wallpapers.json`](site/wallpapers.json) and open a pull request. Working
wallpapers are as useful to file as broken ones. Run `python3 site/validate.py`
first; CI runs the same check.

**Read the list from your own code**: it is one static file, served with
`Access-Control-Allow-Origin: *`, so there is no API to sign up for:

```sh
curl -s https://unhingedsoftware.github.io/kirie/wallpapers.json \
  | jq -r '.wallpapers[] | select(.status == "works") | .id'
```

Branch on the top-level `schema` field — it is bumped if the entry shape ever
changes.

## Build

Rust (stable) plus the system libraries the workspace links against.

```sh
# Arch
sudo pacman -S --needed rust ffmpeg alsa-lib libpulse shaderc glslang \
    wayland libxkbcommon libx11 mpv freetype2 dbus

# Debian/Ubuntu
sudo apt install -y build-essential clang cmake pkg-config \
    libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev \
    libasound2-dev libpulse-dev libshaderc-dev glslang-dev \
    libwayland-dev libxkbcommon-dev libx11-dev libxcb1-dev libfreetype-dev libdbus-1-dev

# Default build (no web backend): lean and always-green.
cargo build --release -p kirie
```

The binary is `target/release/kirie`.

### Web wallpapers (optional)

Web (`"type": "web"`) wallpapers need an embedded browser, behind a cargo
feature. The default build enables neither, so it needs no browser libraries.

| Feature | Backend | System deps |
|---------|---------|-------------|
| `web-cef` | Chromium Embedded Framework (off-screen) | cmake, a C++ toolchain, clang; libcef downloaded on first build |
| `web-webview` | wry + system webkit2gtk-4.1 | `libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev` |

```sh
cargo build --release -p kirie --features web-cef      # bundles CEF, composites via wgpu
cargo build --release -p kirie --features web-webview  # needs webkit2gtk-4.1
```

## Usage

One binary, driven by flags or by its control socket, so a shell or panel can
launch and steer it without a wrapper:

```sh
kirie --screen-root HDMI-A-1 --bg /path/to/workshop/item --scaling fill
kirie info  <item|scene.pkg|.tex>      # inspect
kirie extract <scene.pkg|.tex> -o DIR  # unpack
kirie list                             # what is installed
kirie workshop browse                  # find more, and subscribe
```

The Workshop commands talk to your own running Steam client, so they need
Steam open and an account that owns Wallpaper Engine — Steam enforces that,
kirie does not check licences itself. `kirie workshop search "miku" --tag
Scene` is the same thing without the terminal UI, and every result says
whether this build can render it before you install it.

```sh
kirie workshop subscribe 1388331347 --wait --apply HDMI-A-1
```

subscribes, waits for Steam to fetch it, and shows it — the wallpaper arrives
in Steam's own library, updates with it, and Wallpaper Engine on Windows sees
it too.

## Credits

- Wallpaper Engine is a product of Wallpaper Engine Team; this project is an
  independent, unaffiliated renderer for its content formats.
- Wallpapers on the [compatibility list](https://unhingedsoftware.github.io/kirie/)
  are the work of their Workshop authors.

## License

AGPL-3.0-or-later. See `LICENSE`.

Copyleft: you may use kirie (including commercially) and modify it, but if you
distribute it **or run a modified version as a network service**, you must make
your modified source available under the same license.
