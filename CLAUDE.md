# hyprgallery

A wallpaper gallery for Hyprland and Waybar: a daemon, a Waybar module, and a
GTK4 layer-shell gallery with two layouts. See `docs/ARCHITECTURE.md` for the
shape of it and `docs/THEMING.md` for the selectors, which are public API.

## Layout

- `crates/hyprgallery-core`: no GTK. Library scan, thumbnails, colour, state,
  backends, rotation, the IPC protocol and the `Engine` that ties them together.
  Anything that can be tested without a display belongs here.
- `crates/hyprgallery-ui`: the gallery window. Draws and handles input; every
  change goes through the engine and comes back as `Change`s.
- `crates/hyprgallery`: the binary. CLI, daemon wiring, socket server, Waybar
  module.

## Rules

- Backends build their command lines in `backend::plan` and run them in
  `backend::execute`. New backends get a `plan` test with the exact argv.
- Never hold an `engine.borrow_mut()` across a call that dispatches changes. The
  gallery borrows the engine while handling them. Bind the result to a `let`
  first.
- Animation goes through `motion::Animated`, on the frame clock. No timers for
  anything that moves.
- Do not apply wallpapers from tests. Use the `Command` backend writing to a
  temporary file.

## Building

`/tmp` on this machine is tmpfs, which means RAM. Never build there. Package
builds and any other scratch compilation go under `~/.cache/hyprgallery-build/`
and get removed when finished with.

## Writing

No em dashes in prose, comments, or commit messages. Comments say why, not what.
