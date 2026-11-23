#!/usr/bin/env bash
# Прогоняет каждую игру в нескольких режимах и печатает таблицу.
#
# Зачем: «некоторые игры вылетают» — это не отчёт, с которым можно работать.
# Нужен список: какая игра, в каком режиме, с каким кодом возврата и какой
# последней строкой. Тогда падение воспроизводимо одной командой, а не памятью.
#
#   scripts/games_matrix.sh            # короткий прогон всех игр
#   MODES="short capture" scripts/games_matrix.sh
#   BIN=target/debug/rlisp-engine scripts/games_matrix.sh
set -uo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-target/release/rlisp-engine}"
MODES="${MODES:-short long capture big}"
FRAMES_SHORT="${FRAMES_SHORT:-60}"
FRAMES_LONG="${FRAMES_LONG:-600}"
OUTDIR="${OUTDIR:-/tmp/rlisp-matrix}"
mkdir -p "$OUTDIR"

GAMES=()
for f in assets/lisp/main*.lisp; do
  GAMES+=("$(basename "$f")")
done

if [ ! -x "$BIN" ]; then
  echo "нет бинарника $BIN — сначала cargo build --release" >&2
  exit 2
fi

printf '%-22s %-9s %-4s %s\n' "игра" "режим" "код" "последняя строка"
printf '%.0s-' {1..92}; echo

fails=0
total=0
for game in "${GAMES[@]}"; do
  for mode in $MODES; do
    case "$mode" in
      short)   args=(--fast --frames "$FRAMES_SHORT" --width 320 --height 180) ;;
      long)    args=(--fast --frames "$FRAMES_LONG" --width 320 --height 180) ;;
      capture) args=(--fast --frames 120 --width 640 --height 360 --capture "$OUTDIR/$game") ;;
      big)     args=(--fast --frames 20 --width 1920 --height 1080) ;;
      ascii)   args=(--ascii --fast --frames 60 --stdin-keys) ;;
      *)       echo "неизвестный режим: $mode" >&2; exit 2 ;;
    esac
    log="$OUTDIR/$game.$mode.log"
    timeout 180 "$BIN" --script "assets/lisp/$game" "${args[@]}" < /dev/null > "$log" 2>&1
    code=$?
    total=$((total + 1))
    last="$(tr -d '\r' < "$log" | grep -a -v '^$' | tail -1 | cut -c1-52)"
    if [ "$code" -ne 0 ]; then
      fails=$((fails + 1))
      printf '%-22s %-9s %-4s %s\n' "$game" "$mode" "$code" "!! $last"
      grep -a -m2 -E "panic|thread '|\[ошибка\]" "$log" | sed 's/^/    /'
    else
      printf '%-22s %-9s %-4s %s\n' "$game" "$mode" "$code" "$last"
    fi
  done
done

echo
echo "прогонов: $total, упало: $fails (логи: $OUTDIR)"
[ "$fails" -eq 0 ]
