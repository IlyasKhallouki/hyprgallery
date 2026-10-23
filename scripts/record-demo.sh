#!/usr/bin/env bash
# Record the demo video: the hidden `hyprgallery demo` command run inside a
# private headless weston. Nothing touches the desktop you are sitting at: it
# gets its own Wayland socket, its own hyprgallery socket and its own config,
# state and cache under ~/.cache/hyprgallery-demo.
#
#   scripts/record-demo.sh LIBRARY DESKTOP OUT.mp4 [path/to/hyprgallery]
#
# LIBRARY is a folder of album folders. The scripted tour switches to albums
# called Forests, Space, Dusk and Neon and searches for "sunset", so a library
# with those names and a few sunsets gives the intended video. DESKTOP is the
# wallpaper drawn behind the gallery and the one it opens on, and should be
# inside LIBRARY.
#
# The session runs three times slower than real time and the recorder plays it
# back at true speed, so the video is a smooth 30 fps even on a machine that
# cannot render 1080p that fast. Needs weston and ffmpeg.
set -euo pipefail

library=$(realpath "$1")
desktop=$(realpath "$2")
out=$(realpath -m "$3")
bin=$(realpath "${4:-target/release/hyprgallery}")
scale=${SCALE:-3}
work=${XDG_CACHE_HOME:-$HOME/.cache}/hyprgallery-demo/session
sock=hg-demo-$$

mkdir -p "$work/config/hyprgallery" "$work/state/hyprgallery" "$work/cache"
# Applying sleeps so the tile's spinner is visible: 1.4 s on screen.
sleep_for=$(awk "BEGIN { print 1.4 * $scale }")
cat >"$work/config/hyprgallery/config.toml" <<EOF
backend = "command"
command = "sleep $sleep_for; true {path}"
close_on_apply = false
restore_on_start = false
[library]
roots = ["$library"]
hyde_themes = false
EOF
# Every take starts from the same place.
printf '{"current": "%s"}\n' "$desktop" >"$work/state/hyprgallery/state.json"

cat >"$work/weston.ini" <<EOF
[core]
idle-time=0
[shell]
background-image=$desktop
background-type=scale-crop
panel-position=none
EOF

weston --backend=headless --renderer=gl --width=1920 --height=1080 \
  --socket="$sock" --config="$work/weston.ini" >/dev/null 2>&1 &
wpid=$!
trap 'kill $wpid 2>/dev/null || true' EXIT
for _ in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/$sock" ] && break; sleep 0.1; done

HYPRGALLERY_TIME_SCALE=$scale GTK_SLOWDOWN=$scale \
WAYLAND_DISPLAY=$sock \
HYPRGALLERY_SOCKET=$XDG_RUNTIME_DIR/hg-demo-$$.sock \
XDG_CONFIG_HOME=$work/config XDG_STATE_HOME=$work/state XDG_CACHE_HOME=$work/cache \
  timeout 900 "$bin" demo "$out" --desktop "$desktop"
