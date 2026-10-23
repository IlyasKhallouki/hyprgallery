# Contributing

Bug reports from setups other than HyDE with awww are the most useful thing
right now: hyprpaper, swaybg, swww, multiple monitors, fractional scaling, and
folders with unusual files in them.

## Getting set up

```sh
git clone https://github.com/IlyasKhallouki/hyprgallery
cd hyprgallery
cargo test -p hyprgallery-core
```

The core has no GTK dependency and its tests need nothing else installed
(ffmpeg, if present, is used for video thumbnails). The gallery and the binary
need GTK4 and gtk4-layer-shell:

```sh
# Arch
sudo pacman -S gtk4 gtk4-layer-shell

# Fedora
sudo dnf install gtk4-devel gtk4-layer-shell-devel

# Debian and Ubuntu
sudo apt install libgtk-4-dev libgtk4-layer-shell-dev
```

## Running a second copy

`HYPRGALLERY_SOCKET` points every part at a different socket, so a development
build can run beside an installed one:

```sh
export HYPRGALLERY_SOCKET=$XDG_RUNTIME_DIR/hg-dev.sock
HYPRGALLERY_LOG=debug cargo run -- daemon --no-restore &
cargo run -- toggle
```

`--no-restore` stops the development daemon from reapplying your last
wallpaper when it starts. It still applies whatever you click, so point it at a
test config if that matters: `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and
`XDG_CACHE_HOME` are all respected, and `backend = "command"` with
`command = "echo {path}"` applies nothing at all.

## Looking at the window without a screen

```sh
weston --backend=headless --width=1920 --height=1080 --socket=hg-test &
WAYLAND_DISPLAY=hg-test cargo run -- snapshot /path/to/out.png --view Space --select 3
```

`snapshot` is a hidden command that opens the gallery, waits for every visible
thumbnail, and saves the window as a PNG. Weston has no layer shell, so the
gallery opens as an ordinary full-screen window there; everything inside it is
the same. It is how the screenshots in the README are made, and a quick way to
check a layout or stylesheet change. `--layout popover` and `--peek` cover the
other states.

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets   # CI runs this with -D warnings
cargo test --workspace
```

## What the code expects of you

**Keep the core free of GTK.** If something can be decided without drawing it,
it goes in `hyprgallery-core` with a test.

**Backends are tested by their command lines.** Add a `plan` test with the exact
argv for any backend change. Tests never apply a real wallpaper.

**Never hold an engine borrow across a dispatch.** The gallery reads the engine
while it handles changes. Bind the result of an engine call to a `let`, then
dispatch.

**Animate on the frame clock.** Use `motion::Animated`. A timer-driven animation
judders on high refresh rate displays.

**Widget names are public API.** Anything a stylesheet can select is documented
in [`docs/THEMING.md`](docs/THEMING.md). Renaming one breaks themes, so it counts
as a breaking change.
