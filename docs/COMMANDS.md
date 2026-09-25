# kirie command reference

Every subcommand, flag, control-socket command and environment variable kirie
understands, with a worked example for each.

kirie is one binary with two halves. Subcommands (`kirie list`, `kirie info`,
`kirie workshop …`) inspect and manage wallpapers and exit. Everything else is
read as *renderer* flags — the same spelling
[linux-wallpaperengine](https://github.com/Almamu/linux-wallpaperengine) uses,
so existing scripts and panel configs keep working.

```sh
kirie                 # prints the version and exits
kirie --help          # renderer flag summary
kirie list --help     # clap help for any subcommand
```

The first word decides which half runs: `ask`, `assets`, `check`, `extract`,
`gpus`, `info`, `list`, `preview`, `update` and `workshop` are subcommands, and
anything else goes to the renderer.

## Contents

- [Naming a wallpaper](#naming-a-wallpaper)
- [Subcommands](#subcommands)
  - [`kirie list`](#kirie-list)
  - [`kirie info`](#kirie-info)
  - [`kirie check`](#kirie-check)
  - [`kirie assets`](#kirie-assets)
  - [`kirie gpus`](#kirie-gpus)
  - [`kirie extract`](#kirie-extract)
  - [`kirie workshop`](#kirie-workshop)
  - [`kirie ask`](#kirie-ask)
  - [`kirie preview`](#kirie-preview)
  - [`kirie update`](#kirie-update)
- [Renderer flags](#renderer-flags)
  - [Ordering matters](#ordering-matters)
  - [Where to draw](#where-to-draw)
  - [What to draw](#what-to-draw)
  - [Timing and quality](#timing-and-quality)
  - [Audio](#audio)
  - [Power and pausing](#power-and-pausing)
  - [Input](#input)
  - [Scene properties](#scene-properties)
  - [Screenshots](#screenshots)
  - [Debugging](#debugging)
- [Control socket](#control-socket)
- [Environment variables](#environment-variables)
- [Exit status](#exit-status)

## Naming a wallpaper

Anywhere kirie takes a wallpaper — `--bg`, the trailing positional argument,
`kirie info`, `kirie check` — you can give either:

- a **Workshop ID**, looked up under every Steam library on the machine, or
- a **path** to the item directory (anything containing a `/` is taken as a path).

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347
kirie --screen-root HDMI-A-1 --bg ~/.steam/steam/steamapps/workshop/content/431960/1388331347
```

A bare ID with no `/` that matches no installed item fails with `Cannot find
workshop directory for steam app 431960 and content <id>`.

## Subcommands

### `kirie list`

Every Wallpaper Engine item installed locally, with the type kirie detected and
whether this build can render it.

```
kirie list [--dir <DIR>] [--json]
```

| Flag | Meaning |
|------|---------|
| `--dir <DIR>` | Scan this directory instead of the Steam libraries |
| `--json` | Emit a JSON array instead of the table |

```sh
kirie list
kirie list --dir ~/wallpapers
kirie list --json | jq -r '.[] | select(.renderable) | .id'
```

Each JSON element carries `id`, `title`, `type`, `dir`, `preview`,
`renderable`, `reason` and `update_available`. `type` is one of `scene`,
`video`, `image`, `web`, `application`, `asset` or `unknown`; `reason` says why
an item is not renderable (a web wallpaper on a build with no web feature, for
instance) and is `null` when it is.

### `kirie info`

Print what a wallpaper, a `scene.pkg` or a `.tex` actually contains.

```
kirie info <PATH>
```

The argument may be a workshop item directory, a `project.json`, a `scene.pkg`
or a `.tex` file — kirie sniffs the contents rather than trusting the name.

```sh
kirie info ~/.steam/steam/steamapps/workshop/content/431960/1388331347
kirie info ./1388331347/project.json
kirie info ./1388331347/scene.pkg
kirie info ./materials/water.tex
```

### `kirie check`

Report whether this machine and this build can render at all, and optionally
whether one specific wallpaper will work. Lines are tagged `[ ok ]`, `[warn]`
or `[FAIL]`, and the command exits non-zero if anything failed.

```
kirie check [PATH]
```

```sh
kirie check
kirie check ~/.steam/steam/steamapps/workshop/content/431960/1388331347
```

It checks the GPU adapter, the Wallpaper Engine shared assets, the Steam
Workshop library and which web backend (if any) is compiled in. Run it first
when something does not render — it names the missing piece.

### `kirie assets`

Print the path to the Wallpaper Engine shared assets kirie found. Scene
wallpapers cannot render without them.

```
kirie assets [--json]
```

```sh
kirie assets
kirie assets --json    # {"assets":"/path/…","installed":true}
```

Exits non-zero when no assets were found (unless `--json` is given, which always
succeeds so scripts can read `installed`). Point `KIRIE_WE_ASSETS` at a copy to
override the search.

### `kirie gpus`

List the Vulkan adapters kirie can pin to, with the selector string to pass to
`--gpu`.

```
kirie gpus [--json]
```

```sh
kirie gpus
kirie gpus --json
kirie --gpu nvidia --screen-root HDMI-A-1 --bg 1388331347
```

`auto` is always listed first and means "no pinning".

### `kirie extract`

Unpack a `scene.pkg` archive or decode a `.tex` texture.

```
kirie extract <PATH> [-o|--output <DIR>] [--tex-to-png]
```

| Flag | Default | Meaning |
|------|---------|---------|
| `-o`, `--output <DIR>` | `.` | Where to write the unpacked files |
| `--tex-to-png` | off | Also decode every `.tex` in the package to PNG |

```sh
kirie extract scene.pkg -o /tmp/scene
kirie extract scene.pkg --output /tmp/scene --tex-to-png
kirie extract materials/water.tex -o /tmp/tex
```

Extracting a `project.json` is refused — that is a manifest, not an archive.

### `kirie workshop`

Search, subscribe to and inspect Steam Workshop items. These talk to *your*
running Steam client, so Steam must be open and signed in to an account that
owns Wallpaper Engine. Steam enforces ownership; kirie does not check licences
itself.

Without a usable Steam install every one of them fails the same way, and
`kirie check` says so too:

```
error: no libsteam_api.so in this Steam install (needs a current Steam client)
```

Nothing else is affected — `kirie list`, rendering and screenshots all work
without Steam running.

#### `kirie workshop search`

```
kirie workshop search [TEXT] [--tag <TAG>]... [--exclude-tag <TAG>]... [--any-tag]
                      [--sort <SORT>] [--days <N>] [--page <N>] [--limit <N>] [--json]
```

| Flag | Default | Meaning |
|------|---------|---------|
| `TEXT` | — | Free-text search |
| `--tag <TAG>` | — | Require this tag; repeat for several |
| `--exclude-tag <TAG>` | — | Reject items carrying this tag; repeatable |
| `--any-tag` | off | Match any listed tag instead of all of them |
| `--sort <SORT>` | `popular` | `popular`, `trend`, `recent` or `rated` |
| `--days <N>` | — | Trend window in days, for `--sort trend` |
| `--page <N>` | `1` | Result page; Steam returns up to 50 per page |
| `--limit <N>` | — | Keep only the first N results of the page |
| `--json` | off | Emit a JSON array instead of the table |

```sh
kirie workshop search "miku" --tag Scene
kirie workshop search --tag Scene --exclude-tag Anime --sort recent --limit 10
kirie workshop search "cyberpunk" --sort trend --days 7
kirie workshop search "miku" --json | jq -r '.[] | select(.renderable) | .id'
```

`--sort` also accepts the aliases `subs` (= `popular`), `trending` (= `trend`),
`new` (= `recent`) and `votes` (= `rated`). Every result says whether this build
can render the item *before* you install it.

#### `kirie workshop subscribe`

```
kirie workshop subscribe <ID> [--wait] [--apply <SCREEN>] [--socket <PATH>] [--json]
```

| Flag | Meaning |
|------|---------|
| `--wait` | Block until Steam has finished downloading the item |
| `--apply <SCREEN>` | Show it on that screen once downloaded (implies waiting) |
| `--socket <PATH>` | Control socket of the running kirie to apply through |
| `--json` | Emit the result as JSON |

```sh
kirie workshop subscribe 1388331347
kirie workshop subscribe 1388331347 --wait
kirie workshop subscribe 1388331347 --wait --apply HDMI-A-1
```

The wallpaper lands in Steam's own library, so it updates with Steam and
Wallpaper Engine on Windows sees it too.

#### `kirie workshop unsubscribe`

```
kirie workshop unsubscribe <ID> [--json]
```

```sh
kirie workshop unsubscribe 1388331347
```

#### `kirie workshop state`

Ask Steam what it knows about one item: subscribed, installed, update pending.

```
kirie workshop state <ID> [--json]
```

```sh
kirie workshop state 1388331347
kirie workshop state 1388331347 --json
```

#### `kirie workshop browse`

A terminal UI over the same search, with subscribe built in. Needs the `tui`
cargo feature, which is on by default.

```
kirie workshop browse
```

| Key | Action |
|-----|--------|
| type, `Backspace` | Edit the search text |
| `Enter` | Run the search |
| `↑` / `↓` | Move the selection |
| `PgUp` / `PgDn` | Move ten at a time |
| `←` / `→` | Previous / next page |
| `Tab` | Cycle the sort order |
| `F2` | Subscribe to the selected item |
| `Esc`, `Ctrl-C` | Quit |

### `kirie ask`

Send one line to a running kirie over its control socket and print the reply.
See [Control socket](#control-socket) for the full command list.

```
kirie ask [--socket <PATH>] <WORDS>...
```

```sh
kirie ask ping
kirie ask status
kirie ask bg HDMI-A-1 /path/to/item
kirie ask --socket /run/user/1000/lwe.sock set fps 60
```

Without `--socket` it uses `$XDG_RUNTIME_DIR/lwe.sock`, falling back to a
per-user `0700` directory under the system temp dir when `XDG_RUNTIME_DIR` is
unset (macOS, or a bare login). Windows has no `XDG_RUNTIME_DIR`, so there it is
`%LOCALAPPDATA%\kirie\lwe.sock`, with the same temp-directory fallback when
`LOCALAPPDATA` is unset.

### `kirie preview`

Render one wallpaper off-screen and stream raw RGBA frames over a Unix socket —
how a picker or panel shows a live thumbnail.

```
kirie preview --socket <PATH> --bg <ITEM> [--fps <N>] [--size <PX>]
```

| Flag | Default | Meaning |
|------|---------|---------|
| `--socket <PATH>` | — | Socket to create and stream frames on (required) |
| `--bg <ITEM>` | — | Wallpaper to render (required) |
| `--fps <N>` | `30` | Frame rate of the stream, clamped to 1–120 |
| `--size <PX>` | `960` | Longest edge of the preview, clamped to 64–3840 |

```sh
kirie preview --socket /tmp/kirie-preview.sock --bg 1388331347 --fps 15 --size 480
```

kirie *creates and listens on* the socket; your picker connects to it. Each
frame is a 24-byte little-endian header — the magic `KPV1`, then sequence,
width, height, format and payload length as `u32` — followed by that many bytes
of RGBA8. Format `0` is the only one defined. The aspect ratio follows the
wallpaper, so `--size` caps the longest edge rather than forcing a square.

kirie exits on its own after 30 seconds with no client connected, so a picker
can spawn it per thumbnail and forget about it.

### `kirie update`

Replace this binary with the latest GitHub release.

```
kirie update [--check] [--force]
```

| Flag | Meaning |
|------|---------|
| `--check` | Report what is available, change nothing |
| `--force` | Replace a locally built binary too |

```sh
kirie update --check
kirie update
```

A build made from the repo refuses to update itself unless you pass `--force` —
reinstall from the repo instead. The release asset picked matches the web
feature this build was compiled with.

## Renderer flags

Anything that is not a subcommand runs the renderer. One background is
required: pass it with `--bg`, with `--playlist`, or as a trailing positional
argument.

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347 --scaling fill
```

Most flags may appear only once; `--bg`, `--scaling`, `--clamp`, `--playlist`,
`--screen-root`, `--screen-span`, `--set-property`, `--render-debug` and
`--fullscreen-pause-ignore-appid` may repeat. A repeated single-use flag is a
hard error (`Duplicate argument --fps`). Unknown flags are ignored rather than
rejected.

### Ordering matters

`--bg`, `--scaling`, `--clamp` and `--playlist` apply to the screen named by
the most recent `--screen-root` or `--screen-span`. Put each one *after* the
screen it belongs to:

```sh
# Two screens, two wallpapers, two scaling modes
kirie --screen-root HDMI-A-1 --bg 1388331347 --scaling fill \
      --screen-root DP-2      --bg 3293156956 --scaling fit
```

Before any `--screen-root`, those flags set the window defaults, which each
later screen then inherits.

### Where to draw

| Flag | Default | Meaning |
|------|---------|---------|
| `-r`, `--screen-root <NAME>` | — | Draw on this output as the desktop background; repeatable |
| `--screen-span <A,B,…>` | — | Stretch one wallpaper across several outputs |
| `-w`, `--window <XxYxWxH>` | — | Draw in a normal window instead |
| `--layer <LAYER>` | `bottom` | `background`, `bottom`, `top` or `overlay` |

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347
kirie --screen-span HDMI-A-1,DP-2 --bg 1388331347
kirie --window 0x0x1920x1080 --bg 1388331347
kirie --screen-root HDMI-A-1 --bg 1388331347 --layer background
```

`--window` and `--screen-root` cannot be combined, a screen cannot be named
twice, and a screen already in a span cannot also be given its own
`--screen-root`. A span needs at least two comma-separated names.

### What to draw

| Flag | Default | Meaning |
|------|---------|---------|
| `-b`, `--bg <ITEM>` | — | Workshop ID or path; repeatable, per screen |
| `--playlist <NAME>` | — | A playlist from Wallpaper Engine's `config.json` |
| `--scaling <MODE>` | `default` | `default`, `fit`, `fill` or `stretch` |
| `--clamp <MODE>` | `clamp` | `clamp`, `border` or `repeat` |
| `--focus <X,Y>` | `0,0` | Crop focus, each between -1 and 1 |
| `--assets-dir <DIR>` | — | Wallpaper Engine shared assets to use |

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347 --scaling fill --clamp repeat
kirie --screen-root HDMI-A-1 --bg 1388331347 --scaling fill --focus 0.3,-0.2
kirie --screen-root HDMI-A-1 --playlist "My Playlist"
kirie --assets-dir ~/we-assets --screen-root HDMI-A-1 --bg 1388331347
```

Playlists are read from the Wallpaper Engine `config.json` in your Steam
library, so the name must match one you made in Wallpaper Engine itself. An
unknown name fails with the list of names that do exist.

### Timing and quality

| Flag | Default | Meaning |
|------|---------|---------|
| `-f`, `--fps <N>` | `30` | Target frame rate |
| `--playback-speed <X>` | `1.0` | Animation speed multiplier (alias `--clock`) |
| `--render-scale <X>` | `1.0` | Render below or above native, then scale |
| `--fit-render-to-output` | off | Render at the output size rather than the scene's |
| `--gpu <SELECTOR>` | `auto` | Pin to one Vulkan adapter, from `kirie gpus` |

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347 --fps 60
kirie --screen-root HDMI-A-1 --bg 1388331347 --playback-speed 0.5
kirie --screen-root HDMI-A-1 --bg 1388331347 --render-scale 0.75
kirie --gpu nvidia --screen-root HDMI-A-1 --bg 1388331347
```

`--render-scale` is clamped to 0.5–2.0 when set over the control socket.

### Audio

| Flag | Default | Meaning |
|------|---------|---------|
| `-v`, `--volume <N>` | `15` | Volume, 0–128 |
| `-s`, `--silent` | off | Mute entirely |
| `--noautomute` | off | Keep playing when another app takes audio focus |
| `--no-audio-processing` | off | Skip the audio-reactive analysis |
| `--audio-device <NAME>` | — | Capture from this device instead of the default |

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347 --volume 64
kirie --screen-root HDMI-A-1 --bg 1388331347 --silent
kirie --screen-root HDMI-A-1 --bg 1388331347 --audio-device alsa_output.pci-0000_00_1f.3.analog-stereo.monitor
```

Volumes outside 0–128 are clamped rather than rejected. `--silent` is not
refused alongside `--volume`; it simply mutes on top, so the two together are
silent.

### Power and pausing

| Flag | Default | Meaning |
|------|---------|---------|
| `--no-fullscreen-pause` | off | Keep rendering behind a fullscreen window |
| `--fullscreen-pause-only-active` | off | Only pause for the focused output |
| `--fullscreen-pause-ignore-appid <ID>` | — | Never pause for this app; repeatable |
| `--battery-fps <N>` | `10` | Frame rate while on battery |
| `--release-hidden-after <SECS>` | — | Free GPU memory for a wallpaper hidden this long |

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347 --no-fullscreen-pause
kirie --screen-root HDMI-A-1 --bg 1388331347 --fullscreen-pause-ignore-appid firefox
kirie --screen-root HDMI-A-1 --bg 1388331347 --battery-fps 5 --release-hidden-after 30
```

### Input

| Flag | Meaning |
|------|---------|
| `--interactive` | Let clicks reach the wallpaper |
| `--disable-mouse` | Ignore the pointer entirely |
| `--disable-parallax` | No pointer-driven parallax |
| `--disable-particles` | Skip particle systems |

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347 --interactive
kirie --screen-root HDMI-A-1 --bg 1388331347 --disable-parallax --disable-particles
```

A wallpaper that takes clicks takes them from the desktop too, which is why
`--interactive` is off by default.

### Scene properties

Wallpapers expose author-defined properties — colours, toggles, sliders.

| Flag | Meaning |
|------|---------|
| `-l`, `--list-properties` | Print the properties and exit |
| `--list-properties-json` | Same, as JSON |
| `--set-property <KEY=VALUE>` | Override one property; repeatable (alias `--property`) |

```sh
kirie --bg 1388331347 --list-properties
kirie --bg 1388331347 --list-properties-json | jq
kirie --screen-root HDMI-A-1 --bg 1388331347 --set-property bloom=0 --set-property speed=2
```

A bare key with no `=` is set to `1`, so `--set-property bloom` enables it.

### Screenshots

| Flag | Default | Meaning |
|------|---------|---------|
| `--screenshot <FILE>` | — | Render one frame to a file and exit |
| `--screenshot-delay <SECS>` | `5` | Settle this long before capturing; capped at 600 |

```sh
kirie --bg 1388331347 --screenshot shot.png
kirie --bg 1388331347 --screenshot shot.jpg --screenshot-delay 10
kirie --bg 1388331347 --screenshot shot.png --set-property bloom=0
```

The extension picks the format and must be `.png`, `.jpg`, `.jpeg` or `.bmp`;
anything else is refused before rendering starts. Capture is headless — no
compositor and no `--screen-root` needed, which is what makes it usable over
SSH and in CI.

The canvas is 1280x720 unless the scene declares its own projection size. Set
`KIRIE_SCREENSHOT_SIZE=WxH` to force one:

```sh
KIRIE_SCREENSHOT_SIZE=2560x1440 kirie --bg 1388331347 --screenshot shot.png
```

### Debugging

| Flag | Meaning |
|------|---------|
| `-z`, `--dump-structure` | Print the parsed scene graph |
| `--render-debug <MODE>` | Narrow what gets drawn; repeatable |

`--render-debug` takes `base-only`, `no-solid-final`, `pass-log`,
`pass-readback`, `object=<ID>`, `skip-object=<ID>` or `skip-effect=<ID>`.

```sh
kirie --bg 1388331347 --dump-structure
kirie --bg 1388331347 --screenshot shot.png --render-debug base-only
kirie --bg 1388331347 --screenshot shot.png --render-debug skip-effect=3
```

Set `RUST_LOG=debug` for tracing output alongside any of these.

## Control socket

A running kirie listens on a Unix socket — `$XDG_RUNTIME_DIR/lwe.sock`, or
`%LOCALAPPDATA%\kirie\lwe.sock` on Windows, unless `--control-socket <PATH>`
says otherwise — and reads one command per connection. It is a unix-domain
socket at a path on disk on Windows too, not a named pipe. Use `kirie ask`, or write to it directly with `socat`.

```sh
kirie --screen-root HDMI-A-1 --bg 1388331347 --control-socket /tmp/kirie.sock &
kirie ask --socket /tmp/kirie.sock status
echo "bg HDMI-A-1 /path/to/item" | socat - UNIX-CONNECT:/tmp/kirie.sock
```

`SCREEN` is the output name you gave to `--screen-root`, or `default` in window
mode — `kirie ask status` lists the names in use. It may also be `*`, meaning
every screen; quote it (`'*'`) so the shell does not expand it. `bg` accepts a
bare path with no screen name too, which is the same as `*`.

| Command | Reply | Meaning |
|---------|-------|---------|
| `ping` | `pong` | Liveness check |
| `status` | `speed=…` then one `screen=… bg=…` line each | Current speed and what each screen shows |
| `list` | JSON | Installed wallpapers, as `kirie list --json` |
| `getproperties [SCREEN]` | JSON | Properties of the wallpaper on that screen |
| `bg <SCREEN> <PATH>` | `ok` / `error …` | Swap the wallpaper |
| `preload <PATH>` | `ok` | Warm the cache so the next `bg` is instant |
| `property <SCREEN> <KEY> <VALUE>` | `ok` / `error …` | Set one scene property |
| `stage <KEY> <VALUE>` | `ok` | Stage a property for the next wallpaper |
| `scaling <SCREEN> <MODE>` | `ok` / `error …` | `stretch`, `fit`, `fill` or `default` |
| `clamp <SCREEN> <MODE>` | `ok` / `error …` | `clamp`, `border` or `repeat` |
| `speed <X>` | `ok` | Animation speed; 0 or negative resets to 1 |
| `volume <N>` | `ok` | Volume, 0–128 |
| `mute <0\|1>` | `ok` | Mute or unmute |
| `screenshot <PATH>` | `ok` / `error …` | Capture the live frame |
| `set fps <N>` | `ok` | Target frame rate |
| `set batteryfps <N>` | `ok` | Frame rate on battery |
| `set renderscale <X>` | `ok` | Render scale, clamped to 0.5–2.0 |
| `set audiodevice <NAME>` | `ok` | Capture device; `default` clears it |
| `set noautomute <BOOL>` | `ok` | |
| `set disablemouse <BOOL>` | `ok` | |
| `set disableparallax <BOOL>` | `ok` | |
| `set nofullscreenpause <BOOL>` | `ok` | |
| `workshop search <TEXT>` | JSON | Search the Workshop through Steam |
| `workshop state <ID>` | JSON | What Steam knows about one item |
| `workshop subscribe <ID>` | JSON | Subscribe; returns a job id |
| `workshop unsubscribe <ID>` | JSON | Unsubscribe |
| `workshop job <N>` | JSON | Progress of a subscribe job |

Booleans are `true`/`1` for on, anything else for off. An unrecognised command
answers `unknown command`, and a recognised one with bad arguments answers
`error`. Commands that can fail may also answer `error <reason>` — for example
`error the renderer is not ready yet` when a `bg` swap arrives before the first
frame is on screen. Workshop requests time out after 30 seconds.

```sh
kirie ask speed 0.5
kirie ask volume 64
kirie ask set fps 60
kirie ask preload /path/to/next-item
kirie ask bg '*' /path/to/next-item
kirie ask property HDMI-A-1 bloom 0
kirie ask scaling HDMI-A-1 fill
kirie ask screenshot /tmp/live.png
kirie ask workshop subscribe 1388331347
```

## Environment variables

| Variable | Effect |
|----------|--------|
| `KIRIE_WE_ASSETS` | Path to the Wallpaper Engine shared assets |
| `KIRIE_STEAM_LIBRARY` | Extra Steam library root to search |
| `KIRIE_GPU` | Same as `--gpu`, for the whole process |
| `KIRIE_SCREENSHOT_SIZE` | Screenshot canvas as `WxH`, e.g. `1920x1080` |
| `KIRIE_SCREENSHOT_TIMEOUT_SECS` | How long a capture may take before giving up |
| `KIRIE_AUDIO_PREGAIN`, `KIRIE_AUDIO_BOOST` | Scale the audio-reactive input |
| `KIRIE_BLOOM_THRESHOLD`, `KIRIE_BLOOM_STRENGTH` | Override the bloom pass |
| `KIRIE_NO_PREBAKE` | Skip the prebaked scene-bundle cache |
| `KIRIE_NO_PIPELINE_CACHE` | Skip the on-disk pipeline cache |
| `KIRIE_SHADER_DUMP`, `KIRIE_SHADER_DUMP_ALL` | Write translated shaders to disk |
| `KIRIE_WEB_CONSOLE` | Forward web-wallpaper console output to the log |
| `KIRIE_CORPUS` | Wallpaper corpus directory used by the tests |
| `RUST_LOG` | Tracing filter, e.g. `RUST_LOG=debug` |
| `XDG_RUNTIME_DIR` | Where the default control socket lives |

```sh
KIRIE_WE_ASSETS=~/we-assets kirie --bg 1388331347 --screenshot shot.png
KIRIE_SCREENSHOT_SIZE=2560x1440 kirie --bg 1388331347 --screenshot shot.png
RUST_LOG=debug kirie --screen-root HDMI-A-1 --bg 1388331347
```

## Exit status

`0` on success. `1` on any failure: a wallpaper that could not be found or
rendered, a `kirie check` that reported a `[FAIL]`, a `kirie assets` that found
nothing, a malformed flag, or a control-socket request with no renderer
answering.

```sh
kirie check >/dev/null || echo "this machine cannot render"
```
