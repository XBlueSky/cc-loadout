#!/usr/bin/env bash
# Re-render the README demo GIF: demo/record.sh
# Needs vhs + ttyd + ffmpeg on PATH (hints below). Output: docs/assets/demo.gif
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"

missing=0
for tool in vhs ttyd ffmpeg git; do
  command -v "$tool" > /dev/null || { echo "error: $tool not on PATH" >&2; missing=1; }
done
if [ "$missing" = 1 ]; then
  cat >&2 <<'HINTS'
install hints:
  vhs:    https://github.com/charmbracelet/vhs/releases   (single binary)
  ttyd:   https://github.com/tsl0922/ttyd/releases        (static binary)
  ffmpeg: apt-get install ffmpeg   (needs a build recent enough to have the
          `fillborders` filter — ffmpeg 4+; the old 3.4 series lacks it)
  git:    your system package manager (fixture.sh git-inits the fake repos)
HINTS
  exit 1
fi

cargo build --release
demo/fixture.sh > /dev/null
fx="$repo/target/demo-fixture"

# Smoke gate: the sandbox must be coherent before we point a camera at it.
HOME="$fx" XDG_DATA_HOME="$fx/.local/share" PATH="$fx/bin:$PATH" \
  "$fx/bin/cc-loadout" status --json > /dev/null

sed "s|__FX__|$fx|g" demo/demo.tape > target/demo.tape
mkdir -p docs/assets
PATH="$fx/bin:$PATH" vhs target/demo.tape

gif="docs/assets/demo.gif"
size=$(stat -c%s "$gif")
if [ "$size" -gt $(( 4 * 1024 * 1024 )) ]; then
  echo "error: $gif is $(( size / 1024 )) KiB — over the 4 MiB guard." >&2
  echo "Trim Sleeps in demo/demo.tape or reduce Set Width/Height." >&2
  exit 1
fi
echo "ok: $gif ($(( size / 1024 )) KiB)"
