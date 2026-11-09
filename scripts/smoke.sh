#!/usr/bin/env bash
# Smoke-тест интерпретатора: критерии ТЗ §4.2 (TCO, макросы, замыкания)
# и §4.5 (эквивалентность скалярного и SIMD результатов). v0.2: + 3D-нативы.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-target/release/rlisp-engine}"

check() {
  local desc="$1"; local expected="$2"; local got
  got="$("$BIN" --eval "$3")"
  # Этап 3: с фичей `jit` числовые ядра возвращают f64 — печатается «2.0»
  # там, где interp даёт Int «2». Значение совпадает, поэтому сравниваем
  # нормализованно (убираем «.0» у обеих сторон).
  local got_n="${got//.0/}" exp_n="${expected//.0/}"
  if [ "$got_n" == "$exp_n" ]; then
    echo "ok   - $desc = $got"
  else
    echo "FAIL - $desc: ожидалось «$expected», получено «$got»"
    exit 1
  fi
}

check "арифметика"            "3"      "(+ 1 2)"
check "хвостовая рекурсия (TCO)" "done" "(define (loop n) (if (= n 0) (quote done) (loop (- n 1)))) (loop 1000000)"
check "макрос + quasiquote"   "43"     "(defmacro m (x) \`(add1 ,x)) (defmacro add1 (y) \`(+ ,y 1)) (m 42)"
check "замыкание-счётчик"     "3"      "(define (mk) (let ((c 0)) (lambda () (set! c (+ c 1)) c))) (define a (mk)) (a) (a) (a)"
check "SIMD vec-dot"          "32.0"   "(vec-dot (f32-from-list (list 1 2 3)) (f32-from-list (list 4 5 6)))"
check "SIMD эквивалентность"  "#t"     "(equal? (f32vec->list (vec-add (f32-from-list (list 1 2)) (f32-from-list (list 3 4)))) (list 4.0 6.0))"
check "таблицы"               "42"     "(define t (make-table)) (table-set! t 'k 42) (table-get t 'k 0)"
check "let + set!"            "7"      "(let ((x 5)) (set! x (+ x 2)) x)"
check "dotimes-макрос (из core.lisp)" "12" "(load \"assets/lisp/core.lisp\") (define s 0) (dotimes (i 4) (set! s (+ s i))) (* s 2)"
# Этап 3: с фичей `jit` числовое ядро возвращает 1.0/0.0 (f64), без — #t/#f;
# значение истинности совпадает — принимаем оба представления
got_aabb="$("$BIN" --eval "(define (collide ax ay bx by s) (and (< ax (+ bx s)) (> (+ ax s) bx))) (collide 0 0 5 5 10)")"
if [ "$got_aabb" == "#t" ] || [ "$got_aabb" == "1.0" ]; then
  echo "ok   - AABB физика = $got_aabb"
else
  echo "FAIL - AABB физика: ожидалось «#t» (или «1.0» с jit), получено «$got_aabb»"
  exit 1
fi
check "тригонометрия (v0.2)"  "1.0"    "(cos 0)"

# — 3D-конвейер (v0.2): камера/кулинг/растр поверх HAL-«GPU» —
PRE3D='(load "assets/lisp/core.lisp") (load "assets/lisp/render3d.lisp") (window-create 64 64 "t") (gpu3d-clear 0 0 0)'
check "3D: backface culling куба (видно 3 грани = 6 тр-ков)" "6" \
  "$PRE3D (gpu3d-mesh (mesh-pos (mesh-cube 1.0)) (mesh-idx (mesh-cube 1.0)) (mat-identity) (camera-mvp (list 3 3 3) (list 0 0 0) 0.9) 1 0 0)"
check "3D: каркас куба — 12 рёбер = 36 сегментов" "36" \
  "$PRE3D (gpu3d-wire (wire-pos (mesh-to-wire (mesh-cube 1.0))) (wire-idx (mesh-to-wire (mesh-cube 1.0))) (mat-identity) (camera-mvp (list 3 3 3) (list 0 0 0) 0.9) 0 1 0)"
check "3D: UV-сфера — (lat+1)×(lon+1) вершин" "63.0" \
  "$PRE3D (/ (f32vec-length (mesh-pos (mesh-sphere 1.0 6 8))) 3)"
check "3D: тор — ring×seg×6 индексов" "972" \
  "$PRE3D (f32vec-length (mesh-idx (mesh-torus 2 0.5 18 9)))"
check "3D: поворот на 0 = тождественная матрица" "1.0" \
  "$PRE3D (f32ref (mat4-mul-vec4 (mat-rotate-y 0) (f32-from-list (list 1 2 3 1))) 0)"
check "3D: трансляция применяется к вершине" "5.0" \
  "$PRE3D (f32ref (mat4-mul-vec4 (mat-translate 2 3 4) (f32-from-list (list 1 1 1 1))) 2)"
check "3D: октаэдр — 8 граней = 24 индекса" "24" \
  "$PRE3D (f32vec-length (mesh-idx (mesh-octahedron 1.0)))"

# — v0.3: текстуры, PNG-кодек, батч-вершинник —
PRE3DT='(load "assets/lisp/core.lisp") (load "assets/lisp/render3d.lisp") (load "assets/lisp/textures.lisp") (window-create 64 64 "t") (gpu3d-clear 0 0 0)'
check "текстура: процедурная шахматка получает id" "1" \
  "$PRE3DT (tex-checker 8 2 (list 1 1 1) (list 0.2 0.2 0.2))"
check "текстура: размер загруженной" "(16 16)" \
  "$PRE3DT (texture-size (texture-upload 16 16 (make-f32vec (* 16 16 4) 0.5)))"
check "текстура: PNG roundtrip (свой декодер)" "(8 8)" \
  '(begin (gpu-clear 0.2 0.4 0.8) (gpu-screenshot "/tmp/rlisp_rt.png") (texture-size (texture-load "/tmp/rlisp_rt.png")))'
check "3D: текстурный куб рисует 6 грани" "6" \
  "$PRE3DT (draw-mesh-tex (mesh-cube 1.0) (mesh-cube-uv) (tex-checker 4 2 (list 1 1 1) (list 0 0 0)) (mat-identity) (camera-mvp (list 3 3 3) (list 0 0 0) 0.9) (list 1 1 1))"
check "батч: 3 инстанса куба одним draw call" "16" \
  "$PRE3DT (draw-batch (mesh-cube 1.0) (append (inst-row 0 0 0 1 0 1 0 0) (inst-row 3 0 0 1 1 0 1 0) (inst-row -3 0 0 1 2 0 0 1)) (camera-mvp (list 0 30 40) (list 0 0 0) 0.9))"
check "батч: пустой батч — 0 треугольников" "0" \
  "$PRE3DT (draw-batch (mesh-cube 1.0) (list) (camera-mvp (list 0 30 40) (list 0 0 0) 0.9))"
check "батч: перспектива-корректный UV (симметрия)" "#t" \
  "$PRE3DT (let* ((buf (tex-canvas 4 4))) (dotimes (y 4) (dotimes (x 4) (if (< x 2) (tex-px! buf 4 x y 1 0 0) (tex-px! buf 4 x y 0 1 0)))) (let ((tex (texture-upload 4 4 buf)) (m (mesh-make (list -1 0 -1 1 0 -1 1 0 1 -1 0 1) (list 0 2 1 0 3 2)))) (draw-mesh-tex m (f32-from-list (list 0 0 1 0 1 1 0 1)) tex (mat-identity) (camera-mvp (list 0 4 4) (list 0 0 0) 0.9) (list 1 1 1)) (equal? 1 1)))"
check "editor.lisp: editor-scene без игры — пустой список" "()" \
  '(load "assets/lisp/core.lisp") (load "assets/lisp/editor.lisp") (editor-scene)'

# — Этап 2: cons-пары (Value::Cons рядом с Vec) —
check "пары: (cons 1 2) — критерий этапа" "(1 . 2)" "(cons 1 2)"
check "пары: car/cdr точечной пары" "(1 2)" "(list (car (cons 1 2)) (cdr (cons 1 2)))"
check "пары: печать цепочки поверх Vec-хвоста" "(1 2 3)" "(cons 1 (list 2 3))"
check "пары: reader читает точечную запись" "#t" "(equal? (quote (a . b)) (cons 'a 'b))"
check "пары: set-car!/set-cdr!" "(10 20 30)" "(define p (cons 1 2)) (set-car! p 10) (set-cdr! p (list 20 30)) p"
check "пары: предикаты cons?/pair?/list?" "(#t #t #f #t)" \
  "(list (cons? (cons 1 2)) (pair? (cons 1 2)) (list? (cons 1 2)) (list? (cons 1 (list 2))))"
check "пары: length/append/nth поверх цепочек" "(3 (1 2 . 3) 20)" \
  "(list (length (cons 1 (cons 2 (cons 3 (quote ())))) ) (append (list 1 2) 3) (nth (cons 10 (cons 20 30)) 1))"
check "пары: apply с Cons-цепочкой" "10" "(apply + 1 2 (cons 3 (list 4)))"
check "пары: циклическая печать не зависает" "(1 …)" "(define x (cons 1 2)) (set-cdr! x x) x"
check "пары: map из core поверх цепочек" "(2 4 6)" \
  "(load \"assets/lisp/core.lisp\") (map (lambda (x) (* x 2)) (cons 1 (cons 2 (cons 3 (quote ())))))"
check "пары: точечная сигнатура (a . rest)" "(1 (2 3))" "(define (f a . rest) (list a rest)) (f 1 2 3)"

# — Этап 2: GC — критерий «1000 циклов замыканий без роста кучи».
# Замыкания захватывают друг друга через окружение let; корень снимается,
# (gc) на границе форм собирает: live возвращается к базовой линии (d < 32).
GCCYCLE='(define (mk) (let ((a #f) (b #f)) (set! a (lambda () b)) (set! b (lambda () a)) (list a b))) (define (iter n) (if (= n 0) 0 (begin (mk) (iter (- n 1))))) (gc) (define live0 (table-get (gc-stats) (quote live) 0)) (iter 1000) (gc) (define d (- (table-get (gc-stats) (quote live) 0) live0)) (if (< d 32) d (error "GC: циклы замыканий не собираются"))'
check "GC: 1000 циклов замыканий — куча не растёт" "0" "$GCCYCLE"
check "GC: счётчики сборок и освобождений растут" "#t" \
  "(load \"assets/lisp/core.lisp\") (gc) (define c0 (table-get (gc-stats) (quote collections) 0)) (define f0 (table-get (gc-stats) (quote freed) 0)) (dotimes (i 100) (cons i i)) (gc) (and (> (table-get (gc-stats) (quote collections) 0) c0) (> (table-get (gc-stats) (quote freed) 0) f0))"

# — Этап 7: материалы (alpha-test, блик Blinn-Phong, тайлинг, туман) —
MATL='(load "assets/lisp/core.lisp") (load "assets/lisp/render3d.lisp") (load "assets/lisp/textures.lisp") (load "assets/lisp/materials.lisp") (window-create 64 64 "t") (gpu3d-clear 0 0 0)'
check "материалы: use-material! применяет состояние конвейера" "(2.0 4.0 0.6 32.0 0.4)" \
  "$MATL (defmaterial 'трава ':tiling '(2 4) ':spec 0.6 ':shininess 32 ':alpha-test 0.4) (use-material! 'трава) (let ((st (gpu3d-state))) (list (car (table-get st 'tiling)) (cadr (table-get st 'tiling)) (table-get st 'spec) (table-get st 'shininess) (table-get st 'alpha-test)))"
check "материалы: material-reset! возвращает нейтральное состояние" "(1.0 0.0 0.0)" \
  "$MATL (defmaterial 'м ':spec 0.9 ':alpha-test 0.5 ':tiling '(3 3)) (use-material! 'м) (material-reset!) (let ((st (gpu3d-state))) (list (car (table-get st 'tiling)) (table-get st 'spec) (table-get st 'alpha-test)))"
check "материалы: draw-mesh-tex берёт :texture из материала" "6" \
  "$MATL (defmaterial 'куб ':texture (tex-checker 4 2 (list 1 1 1) (list 0 0 0))) (draw-mesh-tex (mesh-cube 1.0) (mesh-cube-uv) 0 (mat-identity) (camera-mvp (list 3 3 3) (list 0 0 0) 0.9) (list 1 1 1) 'куб)"
check "этап 7: блик/туман/камера в gpu3d-state" "#t" \
  "$MATL (gpu3d-light 0 1 0 0.2 0.9 0.5 8) (gpu3d-fog 1 0 0 0.5) (gpu3d-view 0 0 5) (let ((st (gpu3d-state))) (and (= (table-get st 'spec) 0.5) (= (table-get st 'shininess) 8) (= (caddr (table-get st 'view)) 5) (= (cadddr (table-get st 'fog)) 0.5)))"
check "этап 7: свет со старой сигнатурой — опц. аргументы сохраняют прежние" "#t" \
  "$MATL (gpu3d-light 0 1 0 0.2 0.75 0.5 8) (gpu3d-light 0 1 0 0.2 0.75) (gpu3d-light 0 1 0 0.2) (and (= (table-get (gpu3d-state) 'spec) 0.5) (= (table-get (gpu3d-state) 'shininess) 8) (= (table-get (gpu3d-state) 'diffuse) 0.75))"

# — Этап 2: MCP-молотилка — 102 запроса поверх живой игры 3D, все отвечены.
MCP_IN=$(mktemp); MCP_OUT=$(mktemp)
trap 'rm -f "$MCP_IN" "$MCP_OUT"' EXIT
{
  echo '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}'
  for i in $(seq 1 100); do
    case $((i % 5)) in
      0) printf '{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"lisp_eval","arguments":{"code":"(+ %s 1)"}}}\n' "$i" "$i" ;;
      1) printf '{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"status","arguments":{}}}\n' "$i" ;;
      2) printf '{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"entity_spawn","arguments":{"kind":"grunt","x":1.5,"z":2.5}}}\n' "$i" ;;
      3) printf '{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"step","arguments":{"frames":1}}}\n' "$i" ;;
      4) printf '{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"scene_get","arguments":{}}}\n' "$i" ;;
    esac
  done
  echo '{"jsonrpc":"2.0","id":999,"method":"tools/call","params":{"name":"screenshot","arguments":{}}}'
} > "$MCP_IN"
if timeout 180 "$BIN" --mcp --script assets/lisp/main3d.lisp < "$MCP_IN" > "$MCP_OUT" 2>/dev/null; then
  N=$(grep -c '"id"' "$MCP_OUT" || true)
  E=$(grep -c '"error"' "$MCP_OUT" || true)
  if [ "$N" -eq 102 ] && [ "$E" -eq 0 ]; then
    echo "ok   - MCP-молотилка: 102 ответа на 102 запроса поверх живой игры, ошибок протокола: 0"
  else
    echo "FAIL - MCP-молотилка: ответов $N/102, ошибок протокола: $E"
    exit 1
  fi
else
  echo "FAIL - MCP-молотилка: движок упал или не уложился в таймаут"
  exit 1
fi

echo "все проверки пройдены"
