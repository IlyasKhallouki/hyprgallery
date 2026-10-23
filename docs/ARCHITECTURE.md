# Architecture

hyprgallery is one binary with three jobs: a daemon that owns the gallery
window and all state, a Waybar module that prints that state, and a set of
commands that ask the daemon to do things.

```
 Waybar ──exec──▶ hyprgallery bar ──subscribe──┐
 Waybar ──click─▶ hyprgallery toggle ─request──┤
 you    ────────▶ hyprgallery set|next|rotate ─┤
                                               ▼
                        $XDG_RUNTIME_DIR/hyprgallery.sock
                                               │
 ┌────────────────────── hyprgallery daemon ───┴────────────────────────────┐
 │  socket thread ──▶ GTK main loop ◀── worker threads (thumbnails, apply)  │
 │                      │  Engine (library, metas, state, backend)          │
 │                      │  Gallery (layer surface, two layouts)             │
 │                      ▼                                                   │
 │              awww / swww / hyprpaper / swaybg / mpvpaper / hyde-shell    │
 └──────────────────────────────────────────────────────────────────────────┘
```

## Crates

**`hyprgallery-core`** has no GTK dependency. It holds everything that can be
tested without a display:

- `library`: scans the configured folders. Each folder holding images is an
  album; files directly in a root form an album named after the root; on HyDE,
  each theme's `wallpapers` folder is an album too. Scanning reads directory
  entries and file metadata only, so it is cheap enough to redo on every change.
- `thumbs` and `decode`: each file is decoded once into a 512x288 JPEG
  thumbnail and a `Meta` (size, dominant colour, hue bucket, whether it is
  animated, and a 48x27 blurred copy used as the backdrop). JPEGs are decoded at
  1/2, 1/4 or 1/8 scale inside the IDCT when that still covers the thumbnail,
  which is most of the cost of indexing a folder of camera-sized images. Cache
  keys hash the path, size and mtime, not the contents.
- `colour`: k-means over a small sample with farthest-point seeding, scored so a
  large, saturated area wins over a large grey one or a small bright one.
- `view`: folder albums and the smart albums (Favourites, Recent, By colour,
  Animated), each resolved to an ordered list of wallpapers.
- `backend`: detection and the exact commands for each wallpaper program.
  `plan` builds them and `execute` runs them, so every command line is unit
  tested without running anything.
- `rotation`, `state`, `config`, `search`, `ipc`.
- `engine`: owns all of the above on one thread. Slow work goes to worker
  threads and comes back as `Event`s; every mutation returns the `Change`s it
  caused.

**`hyprgallery-ui`** is the GTK4 window. It reads from the engine to draw and
calls the engine to act, then hands the resulting changes to a `Dispatch`
callback supplied by the daemon.

**`hyprgallery`** is the binary: the CLI, the socket server, the Waybar module,
and the daemon that wires the engine and the window together on GTK's main
loop.

## The window

One layer surface (namespace `hyprgallery`) on the overlay layer, anchored to
all four edges, holds both layouts in a stack:

- **Overlay**: exclusive zone -1, so it covers the bar too. A backdrop widget
  draws the focused wallpaper's blurred copy across the screen and crossfades
  as focus moves; peeking (holding Space) fades in a full-resolution decode of
  the real image and fades the interface out.
- **Popover**: exclusive zone 0, so the bar stays clickable. A panel sits at the
  configured corner; the transparent rest of the surface closes it when
  clicked.

Both layouts share one `gio::ListStore` and one `SingleSelection`, so switching
keeps your place. The surface is built at startup and hidden between uses.
Opening it is a `present()` and a fade, with no GTK start-up cost.

Tiles are a custom widget that draws its own picture, name, badges and spinner.
Their CSS box (corners, outline, shadow) is still styled normally. Every
animation is driven by the frame clock through `motion::Animated`, so it stays
smooth at any refresh rate and can reverse mid-flight.

Textures are decoded off the main thread: two workers for thumbnails and
backdrops, and a separate one for peek images, so a slow 8K decode never holds
up the grid. Each kind has a small LRU cache. A peek request that focus has
already moved past is dropped before it is decoded.

## Applying a wallpaper

`Engine::apply` plans the backend's commands and runs them on a worker thread,
reporting back with `Event::Applied`. While one apply is running, further
requests collapse into a single queued slot, so clicking through ten
wallpapers quickly applies the first and the last, not all ten.

When the gallery covers the whole output, the click position (or the chosen
tile's centre, from the keyboard) is passed to awww as the transition origin,
so `grow` starts from the thumbnail you picked.

Backends that do not restore themselves at login get it from the daemon, which
reapplies the last wallpaper on start unless awww is already showing it. HyDE
restores its own.

## The socket

Newline-delimited JSON over a Unix socket at
`$XDG_RUNTIME_DIR/hyprgallery.sock`. One request and one reply per connection,
except `subscribe`, which keeps the connection and receives a `Status` line
whenever it changes. A lock file next to the socket keeps two daemons started
at the same moment from fighting over it.

## Files

| Path | What |
| --- | --- |
| `~/.config/hyprgallery/config.toml` | Configuration, written commented-out on first run, reloaded on change. |
| `~/.config/hyprgallery/style.css` | Your stylesheet, reloaded on change. |
| `~/.local/state/hyprgallery/state.json` | Current wallpaper, favourites, rotation, last album. |
| `~/.local/state/hyprgallery/daemon.log` | Output of a daemon started by a client. |
| `~/.cache/hyprgallery/` | Thumbnails and `meta.json`. Safe to delete. |
