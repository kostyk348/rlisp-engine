#!/usr/bin/env bash
# demo_island.sh — демо «ОСТРОВ» (этап 8): сборка, захват кадров, GIF.
# Запуск: bash scripts/demo_island.sh
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.local/rust/bin:$PATH"
BIN=target/release/rlisp-engine
[ -x "$BIN" ] || cargo build --release
rm -rf demo/frames_island && mkdir -p demo/frames_island
"$BIN" --script assets/lisp/main_island.lisp \
       --capture demo/frames_island --every 6 --frames 240 --fast \
       2>&1 | tee demo/run_island.log | grep -E "ISLAND fps" || true
python3 /home/z/my-project/work/portfolio/rlisp-engine/scripts/collect_island_demo.py
rm -rf demo/frames_island
echo "[demo] готово: demo/island.gif + filmstrip + стиллы + run_island.log"
