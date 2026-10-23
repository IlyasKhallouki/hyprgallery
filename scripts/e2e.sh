#!/usr/bin/env bash
# End-to-end check of the daemon, the socket and the CLI, without touching the
# desktop you are sitting at.
#
# Everything runs against a private headless weston, a private socket, and
# private config, state and cache directories. The wallpaper backend is a shell
# command that appends to a file, so nothing is ever put on a real screen.
#
#   scripts/e2e.sh [path/to/hyprgallery]
set -euo pipefail

bin=${1:-target/debug/hyprgallery}
bin=$(realpath "$bin")
command -v weston >/dev/null || { echo "e2e: needs weston for a headless display" >&2; exit 2; }

work=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/hyprgallery-e2e.XXXXXX")
sock=hg-e2e-wayland-$$
cleanup() {
  local code=$?
  if [ "$code" -ne 0 ] && [ -f "$XDG_STATE_HOME/hyprgallery/daemon.log" ]; then
    echo "--- daemon.log" >&2
    tail -n 40 "$XDG_STATE_HOME/hyprgallery/daemon.log" >&2
  fi
  "$bin" quit >/dev/null 2>&1 || true
  [ -n "${wpid:-}" ] && kill "$wpid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

export XDG_CONFIG_HOME=$work/config XDG_STATE_HOME=$work/state XDG_CACHE_HOME=$work/cache
export HYPRGALLERY_SOCKET=$XDG_RUNTIME_DIR/hg-e2e-$$.sock
export HYPRGALLERY_LOG=${HYPRGALLERY_LOG:-debug}
applied=$work/applied.txt
mkdir -p "$XDG_CONFIG_HOME/hyprgallery"
cat >"$XDG_CONFIG_HOME/hyprgallery/config.toml" <<EOF
backend = "command"
command = "printf '%s\\n' {path} >> '$applied'"
restore_on_start = false
[library]
roots = ["$work/walls"]
hyde_themes = false
EOF

# A small library: two albums and a loose file. ImageMagick draws them, with
# ffmpeg as the fallback.
make_image() {
  if command -v magick >/dev/null; then
    magick -size 640x360 "gradient:$2" "$1"
  else
    ffmpeg -v error -f lavfi -i "color=c=$3:s=640x360" -frames:v 1 "$1"
  fi
}
mkdir -p "$work/walls/Space" "$work/walls/City"
make_image "$work/walls/Space/earth.png" "navy-black" navy
make_image "$work/walls/Space/moon.png" "gray-black" gray
make_image "$work/walls/City/neon.png" "magenta-purple" magenta
make_image "$work/walls/loose.png" "orange-red" orange

weston --backend=headless --renderer=gl --width=1280 --height=720 --socket="$sock" --idle-time=0 >/dev/null 2>&1 &
wpid=$!
for _ in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/$sock" ] && break; sleep 0.1; done
export WAYLAND_DISPLAY=$sock

pass=0
check() {
  local what=$1; shift
  if "$@"; then pass=$((pass + 1)); echo "ok    $what"; else echo "FAIL  $what" >&2; exit 1; fi
}
status_has() { "$bin" status --json | grep -q "$1"; }

check "status says not running before anything starts" sh -c "'$bin' status | grep -q 'not running'"
check "toggle starts the daemon and opens the gallery" "$bin" toggle
check "the gallery reports itself visible" status_has '"visible": true'
check "toggle closes it again" "$bin" toggle
sleep 0.3
check "and it reports itself hidden" status_has '"visible": false'

check "set applies a file through the backend" "$bin" set "$work/walls/Space/earth.png"
check "the backend received the path" grep -qx "$work/walls/Space/earth.png" "$applied"
check "status names the current wallpaper" status_has '"name": "Earth"'

check "next moves along the album" "$bin" next
sleep 0.5
check "to the moon" grep -qx "$work/walls/Space/moon.png" "$applied"

check "set refuses a missing file" sh -c "! '$bin' set '$work/nope.png' 2>/dev/null"

check "rotate starts a rotation" sh -c "'$bin' rotate 15m --album City | grep -q 'rotation: City every 15 min'"
check "rotate off stops it" sh -c "'$bin' rotate off | grep -q 'rotation: off'"
check "rotate rejects nonsense" sh -c "! '$bin' rotate 5x 2>/dev/null"

check "the bar module prints JSON" sh -c "timeout 2 '$bin' bar | head -1 | grep -q '\"alt\"'"

mkdir -p "$work/walls/Forest"
make_image "$work/walls/Forest/pine.png" "green-black" green
sleep 1.5
check "new folders are picked up without reindexing" sh -c "'$bin' random --album Forest && sleep 0.5 && grep -qx '$work/walls/Forest/pine.png' '$applied'"

check "quit stops the daemon" "$bin" quit
sleep 0.3
check "and status agrees" sh -c "'$bin' status | grep -q 'not running'"

echo "e2e: $pass checks passed"
