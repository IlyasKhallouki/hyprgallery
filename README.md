# hyprgallery

A wallpaper gallery for Hyprland and Waybar.

Point it at a folder of wallpapers and every folder inside it becomes an album.
Click the bar icon and the gallery fills the screen. Its background turns into a
soft blur of whichever wallpaper you are looking at, holding Space shows that
wallpaper sharp and full size, and a click applies it.

![The full-screen gallery, browsing an album](docs/images/overlay.png)

Written in Rust with GTK4 and gtk4-layer-shell. It drives the wallpaper program
you already use: HyDE's scripts, awww, swww, hyprpaper, swaybg, or mpvpaper for
video.

## Why this exists

Most wallpaper pickers show everything in one flat list, and the only way to see
how a wallpaper looks on your screen is to set it. On HyDE that costs more than
it sounds, because setting a wallpaper also recolours the whole theme, which
takes a few seconds and changes your bar, terminal and launcher along with it.

hyprgallery keeps the folders you already sort wallpapers into, and lets you
look before you commit. Nothing on your desktop changes until you click.

## What it does

- Each folder is an album, and images directly inside the root form one of their
  own. On HyDE, every theme's wallpapers appear as an album too.
- Four smart albums sit in front of your folders: Favourites, Recent, Animated,
  and By colour, which sorts everything into a rainbow and filters by hue.
- There are two layouts. The overlay covers the screen; the popover hangs from
  the bar like a menu. They share one selection, so switching keeps your place.
- You can look before you apply. The backdrop follows focus as you move around,
  and holding Space shows the real image at screen size.
- The selection ring and the active album take their colour from the focused
  wallpaper.
- Search covers every album, matching names, album names and file names.
- Rotation changes the wallpaper on a timer, through an album in order or
  shuffled, and carries on after a restart.
- You can drag images in to add them, move a wallpaper to another album, send it
  to the trash, or show it in your file manager.
- GIF and video wallpapers work, and their previews play inside the grid.
- A Waybar module shows what is going on and opens the gallery, and a command
  line covers scripts and key bindings.

![The popover layout under the bar](docs/images/popover.png)

## Install

You need Rust 1.92 or newer, GTK 4.12 or newer, and gtk4-layer-shell.

```sh
# Arch
sudo pacman -S gtk4 gtk4-layer-shell

# Fedora
sudo dnf install gtk4-devel gtk4-layer-shell-devel

# Debian and Ubuntu
sudo apt install libgtk-4-dev libgtk4-layer-shell-dev
```

```sh
git clone https://github.com/IlyasKhallouki/hyprgallery
cd hyprgallery
cargo install --path crates/hyprgallery
```

On Arch, [`contrib/PKGBUILD`](contrib/PKGBUILD) builds a package from a release
tag.

Optional, depending on how you set wallpapers:

| Package | For |
| --- | --- |
| `awww` or `swww` | Animated transitions. `grow` starts from the thumbnail you clicked. |
| `mpvpaper` | Video wallpapers. |
| `ffmpeg` | Thumbnails of video wallpapers. |
| `gst-plugins-good`, `gst-libav` | Playing GIF and video previews inside the gallery. |

### Make sure Waybar can find it

Waybar runs modules with the PATH of the session that started it, which usually
does not include `~/.cargo/bin`. The binary works when you type it and is
invisible to Waybar. Either link it somewhere on the session's PATH:

```sh
ln -s ~/.cargo/bin/hyprgallery ~/.local/bin/hyprgallery
```

or use the full path in the Waybar config below.

## Set up

### Waybar

Add the module from
[`contrib/waybar/custom-hyprgallery.jsonc`](contrib/waybar/custom-hyprgallery.jsonc)
to your config and put `"custom/hyprgallery"` in one of your `modules-*`
lists. Left click opens the full-screen gallery and right click opens the
popover. [`contrib/waybar/hyprgallery.css`](contrib/waybar/hyprgallery.css) has
optional styling for its states.

### Hyprland

Hyprland animates layer surfaces, and hyprgallery animates itself, so turn
Hyprland's animation off for it. The lines are in
[`contrib/hyprland/hyprgallery.conf`](contrib/hyprland/hyprgallery.conf):

```ini
layerrule = no_anim true,match:namespace hyprgallery
```

On Hyprland before 0.53 the syntax is `layerrule = noanim, hyprgallery`.

The daemon starts itself the first time the bar module or a command needs it.
To have thumbnails ready before your first click, start it at login:

```ini
exec-once = hyprgallery daemon
```

### Your wallpapers

hyprgallery reads `~/Pictures/Wallpapers` unless you configure other folders.
Make a folder in it for each album. New files and folders show up on their own
while the gallery is running.

## Using it

| Key | What it does |
| --- | --- |
| Arrows, or `h` `j` `k` `l` | Move around the grid |
| `Enter`, or a click | Apply the wallpaper |
| `Space`, held | Show it full screen while held |
| `F` | Add to or remove from Favourites |
| `/` or `Ctrl+F` | Search every album |
| `Tab`, `Shift+Tab` | Next or previous album |
| `1` to `9` | Jump to an album |
| `R` | Jump to a random wallpaper in the album |
| `M`, or right click | Menu: move to an album, show in folder, trash |
| `O` | Show in the file manager |
| `Delete` | Move to the trash |
| `Ctrl+R` | Rotation menu |
| `Esc` | Clear the search, then close |

Clicking anywhere outside the grid closes the gallery. Dropping image files on it
copies them into the album on screen.

![Holding Space shows the wallpaper full size](docs/images/peek.png)

## How wallpapers are applied

hyprgallery does not draw wallpapers. It finds the program you use and drives
it, in this order unless `backend` in the config says otherwise:

| Backend | How | At login |
| --- | --- | --- |
| HyDE | `hyde-shell wallpaper --global --set`, so wallbash recolours the theme | HyDE restores it |
| hyprpaper, when running | `hyprctl hyprpaper wallpaper` on every monitor | hyprgallery restores it |
| awww, then swww | `awww img` with your transition, starting from the thumbnail you picked | awww restores it; hyprgallery checks |
| swaybg | Starts a new swaybg, then stops the old one, so there is no blank frame | hyprgallery restores it |
| command | Your own command, with `{path}` passed as an argument | hyprgallery restores it |

Videos play through mpvpaper (on HyDE, through HyDE's own mpvpaper backend),
and choosing a still image stops it again. A custom command receives videos like
any other file.

If you click through several wallpapers while one is still being applied, the
gallery finishes the first and then applies only the last one you chose, so HyDE
does not recolour the theme for choices you have already moved past.

## Command line

```text
hyprgallery                         open or close the gallery
hyprgallery toggle [--layout overlay|popover]
hyprgallery show | hide
hyprgallery set FILE                apply a file and wait until it is on screen
hyprgallery next | prev | random [--album NAME]
hyprgallery rotate 30m [--album NAME] [--shuffle]
hyprgallery rotate off
hyprgallery status [--json]
hyprgallery reindex | restore | quit
hyprgallery bar                     the Waybar module
```

`--album` takes a folder name, or `favourites`, `recent`, `colour` or
`animated`. Without it, commands use the album of the current wallpaper.

## Configuration

`~/.config/hyprgallery/config.toml` is written on first run with every option
present and commented out, so the file documents itself. The daemon reloads it
when you save. The options people change most:

```toml
layout = "overlay"            # or "popover", for plain `hyprgallery toggle`
backend = "auto"              # hyde, awww, swww, hyprpaper, swaybg, command
close_on_apply = true

[library]
roots = ["~/Pictures/Wallpapers", "~/Art"]
hyde_themes = true

[transition]
type = "grow"
duration = 0.9

[popover]
anchor = "top-right"
width = 820
height = 600
```

## Theming

The gallery is styled with GTK4 CSS. Put rules in
`~/.config/hyprgallery/style.css` and they apply as soon as you save.
[`docs/THEMING.md`](docs/THEMING.md) lists every selector.

## How it works

One binary does three jobs. `hyprgallery daemon` owns the gallery window and all
state, `hyprgallery bar` streams that state to Waybar, and every other command
sends one request over a Unix socket. The window is created once and kept hidden
between uses, so opening it is a fade rather than a program starting.

Each wallpaper is decoded once into a thumbnail, its dominant colour and a tiny
blurred copy for the backdrop. Large JPEGs are decoded at a fraction of their
size, which is most of the cost of indexing a folder of camera-sized images.
Thumbnails and previews load on worker threads, and every animation runs on the
display's frame clock.

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) has the details.

## Roadmap

- A different wallpaper on each monitor.
- Sharper thumbnails on HiDPI displays.
- Sorting an album by name, date added or colour.

## License

MIT
