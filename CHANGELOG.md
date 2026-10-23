# Changelog

All notable changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Widget names in
[`docs/THEMING.md`](docs/THEMING.md) and the socket protocol count as public
API.

## [Unreleased]

### Added

- Albums from folders, plus Favourites, Recent, By colour and Animated.
- A full-screen overlay layout with a backdrop that follows focus, and hold
  Space to preview a wallpaper full screen before applying it.
- A popover layout that hangs from the bar, with album covers.
- Backends: HyDE, awww, swww, hyprpaper, swaybg, mpvpaper for video, and a
  command of your own. Detected automatically.
- Timed rotation per album, sequential or shuffled, surviving restarts.
- Fuzzy search across every album.
- Drag files in to add them, move between albums, send to the trash, and show
  in the file manager.
- GIF and video wallpapers, with previews that play in the grid.
- A Waybar module that streams state, and a CLI for scripting.
- An accent colour taken from the focused wallpaper, and a stylesheet you can
  override and live-reload.
