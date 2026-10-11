#!/bin/sh
# Regenerate assets/screenshot.png (needs: cargo build --release, freeze).
set -e
cd "$(dirname "$0")/.."
test -x target/release/kara || cargo build --release
freeze --execute "sh -c './target/release/kara --no-pager < assets/demo.diff'" \
  -o assets/screenshot.png \
  --window \
  --background '#0d1117' \
  -W 780 \
  -p 16 \
  -m 30 \
  --border.radius 8
