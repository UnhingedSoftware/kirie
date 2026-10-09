# kirie packages (`.kpk`)

A `.kpk` file is one wallpaper: its files, and a manifest saying what it is.
The byte layout is described at the top of `crates/kirie-pack/src/lib.rs`.

## Making one

Put the wallpaper's files in a folder with a `kirie.json` beside them, then:

```sh
kirie pack my-wallpaper/                 # writes my-wallpaper.kpk
kirie pack my-wallpaper/ -o out.kpk
kirie pack --inspect out.kpk             # show it and check every hash
```

Every file in the folder goes in under its path relative to the folder,
except `kirie.json` itself, hidden files (names starting with `.`) and other
`.kpk` files. Already-compressed formats (video, PNG, JPEG, KTX2 and so on)
are stored as they are; everything else is LZ4-compressed when that saves at
least an eighth. The same folder always gives the same bytes.

## `kirie.json`

```json
{
  "id": "rain-city",
  "title": "Rain city",
  "description": "Neon streets in the rain",
  "author": "you",
  "kind": "video",
  "entry": "loop.mp4",
  "preview": "preview.jpg",
  "tags": ["city", "rain"],
  "mature": false,
  "min_kirie": "0.9.0",
  "properties": [
    { "key": "speed", "label": "Speed", "type": "slider", "min": 0, "max": 2, "step": 0.1, "default": 1 },
    { "key": "rain",  "label": "Rain",  "type": "toggle", "default": true },
    { "key": "tint",  "label": "Tint",  "type": "color",  "default": [1, 0.8, 0.6] },
    { "key": "mode",  "label": "Mode",  "type": "choice", "default": "night",
      "options": [{ "value": "day", "label": "Day" }, { "value": "night", "label": "Night" }] },
    { "key": "motto", "label": "Motto", "type": "text",   "default": "" }
  ]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Stays the same across updates. `a-z`, `0-9`, `-`, `_`, `.`; up to 128 characters |
| `title` | yes | Shown in the library and on the Workshop |
| `kind` | yes | `video`, `image`, `web` or `scene`; `wallpaper_engine` is for converted items only |
| `entry` | yes | The file the player starts from. Video: `.mp4`, `.webm`, `.mkv`. Image: `.png`, `.jpg`, `.webp`, `.ktx2`. Web: `.html`. Scene: `.kscene`. Wallpaper Engine: `project.json` |
| `preview` | no | Image shown in the library and on the Workshop |
| `properties` | no | Settings the user can change, in display order. Colours are linear RGB from 0 to 1 |
| `min_kirie` | no | Oldest kirie that can play it |
| `provenance` | no | Omit for your own work. Converters set `{"origin": "converted", ...}`, and such packages cannot be published |

`kirie pack` refuses a misspelt field name. Players ignore fields they do not
know, so a package made for a newer kirie still lists in an older haru.

The `scene` kind is reserved: the `.kscene` scene format is not defined yet,
so kirie refuses to play one.

## Wallpaper Engine items

```sh
kirie convert ~/.steam/steam/steamapps/workshop/content/431960/1388331347
```

repacks a Wallpaper Engine item, every file as it is, into `we-1388331347.kpk`
(or the path given with `-o`). Its kind is `wallpaper_engine`, its entry is the
item's `project.json`, and its provenance is `converted` with the Workshop id,
so it stays on the machine that made it and can never be published. Scenes
still need Wallpaper Engine's assets, exactly as the item's own folder does.

## Playing a package

`kirie --bg wallpaper.kpk` (and `bg` on the control socket) plays
a package of any kind but `scene`. kirie unpacks it once, checking every entry
against its hash, into `kirie/packages/<fingerprint>` under the user's cache
directory, and plays it from there; the same package starts from that copy
next time. Only the four most recently unpacked packages are kept.
