;; render3d.lisp — Уровень 3: 3D DSL поверх «GPU» HAL (render3d.rs).
;; ВЕСЬ 3D-пайплайн выше треугольников — здесь, на Lisp:
;;   матрицы 4x4 → камера lookAt → перспектива → модели (translate/rotate/scale)
;;   → примитивы (куб/сфера/тор/октаэдр/плоскость) → draw-mesh.
;; Rust знает только треугольники, глубину и свет — как настоящий GPU.
;;
;; Соглашения: правые координаты, Y вверх, CCW-наружу (front faces),
;; матрицы — F32Vec из 16 чисел (row-major), vec4 — столбец (как mat4-mul в SAL).

(define (cadddr x) (car (cdr (cdr (cdr x)))))
(define (cdddr x) (cdr (cdr (cdr x))))

;; ——— матрицы 4x4 ———
(define (m4 . xs) (f32-from-list xs))

(define (mat-identity)
  (m4 1 0 0 0   0 1 0 0   0 0 1 0   0 0 0 1))

;; column-vector конвенция: трансляция — в ПОСЛЕДНЕМ СТОЛБЦЕ (индексы 3 7 11)
(define (mat-translate x y z)
  (m4 1 0 0 x   0 1 0 y   0 0 1 z   0 0 0 1))

(define (mat-scale sx sy sz)
  (m4 sx 0 0 0   0 sy 0 0   0 0 sz 0   0 0 0 1))

(define (mat-rotate-x a)
  (let ((c (cos a)) (s (sin a)))
    (m4 1 0 0 0   0 c (- s) 0   0 s c 0   0 0 0 1)))

(define (mat-rotate-y a)
  (let ((c (cos a)) (s (sin a)))
    (m4 c 0 s 0   0 1 0 0   (- s) 0 c 0   0 0 0 1)))

(define (mat-rotate-z a)
  (let ((c (cos a)) (s (sin a)))
    (m4 c (- s) 0 0   s c 0 0   0 0 1 0   0 0 0 1)))

;; перспектива: fov — угол по вертикали (радианы)
(define (mat-perspective fov aspect near far)
  (let ((f (/ 1.0 (tan (* 0.5 fov)))))
    (m4 (/ f aspect) 0 0 0
        0 f 0 0
        0 0 (/ (+ far near) (- near far)) (/ (* 2 far near) (- near far))
        0 0 -1 0)))

;; ——— векторная математика v3 (не горячая: раз в кадр) ———
(define (v3-sub a b) (list (- (car a) (car b)) (- (cadr a) (cadr b)) (- (caddr a) (caddr b))))
(define (v3-dot a b) (+ (* (car a) (car b)) (* (cadr a) (cadr b)) (* (caddr a) (caddr b))))
(define (v3-cross a b)
  (list (- (* (cadr a) (caddr b)) (* (caddr a) (cadr b)))
        (- (* (caddr a) (car b)) (* (car a) (caddr b)))
        (- (* (car a) (cadr b)) (* (cadr a) (car b)))))
(define (v3-len a) (sqrt (v3-dot a a)))
(define (v3-norm a)
  (let ((l (max 1e-9 (v3-len a))))
    (list (/ (car a) l) (/ (cadr a) l) (/ (caddr a) l))))

;; камера lookAt: мировые → видовые (строки: right, up, -forward)
(define (mat-lookat eye target up)
  (let* ((f (v3-norm (v3-sub target eye)))
         (r (v3-norm (v3-cross f up)))
         (u (v3-cross r f)))
    (m4 (car r) (cadr r) (caddr r) (- (v3-dot r eye))
        (car u) (cadr u) (caddr u) (- (v3-dot u eye))
        (- (car f)) (- (cadr f)) (- (caddr f)) (v3-dot f eye)
        0 0 0 1)))

;; MVP одной матрицей: perspective · lookat
(define (camera-mvp eye target fov)
  (let* ((wh (window-size))
         (aspect (/ (car wh) (cadr wh)))
         (view (mat-lookat eye target (list 0 1 0)))
         (proj (mat-perspective fov aspect 0.5 400.0)))
    (mat4-mul proj view)))

;; ——— меши: (позиции-f32 индексы-f32) ———
(define (mesh-make pos idx)
  (list (f32-from-list pos) (f32-from-list idx)))
(define (mesh-pos m) (car m))
(define (mesh-idx m) (cadr m))

;; quad a b c d (CCW снаружи) → треугольники (a b c) (a c d)
(define (mesh-quads-idx quads)
  (if (null? quads) '()
      (let ((q (car quads)))
        (append (list (car q) (cadr q) (caddr q)
                      (car q) (caddr q) (cadddr q))
                (mesh-quads-idx (cdr quads))))))

(define (mesh-cube s)
  (let ((v (list (- s) (- s) s    s (- s) s    s s s    (- s) s s        ; +Z
                 s (- s) (- s)   (- s) (- s) (- s)   (- s) s (- s)   s s (- s) ; -Z
                 s (- s) s       s (- s) (- s)  s s (- s)  s s s         ; +X
                 (- s) (- s) (- s)  (- s) (- s) s  (- s) s s  (- s) s (- s) ; -X
                 (- s) s s       s s s        s s (- s)  (- s) s (- s)  ; +Y
                 (- s) (- s) (- s)  s (- s) (- s)  s (- s) s  (- s) (- s) s)) ; -Y
        (idx (mesh-quads-idx
              (list (list 0 1 2 3) (list 4 5 6 7) (list 8 9 10 11)
                    (list 12 13 14 15) (list 16 17 18 19) (list 20 21 22 23)))))
    (mesh-make v idx)))

;; октаэдр: 6 вершин, 8 граней (пули, эффекты, каркас)
(define (mesh-octahedron s)
  (let ((v (list s 0 0   (- s) 0 0   0 s 0   0 (- s) 0   0 0 s   0 0 (- s)))
        (idx (list 0 2 4  2 1 4  1 3 4  3 0 4
                   2 0 5  1 2 5  3 1 5  0 3 5)))
    (mesh-make v idx)))

;; UV-сфера: lat колец, lon сегментов. Внутренние define работают как letrec.
(define (mesh-sphere r lat lon)
  (define (vert i j)
    (let ((phi (- (* 3.14159265 (/ i lat)) (/ 3.14159265 2)))
          (th (* 6.28318530 (/ j lon))))
      (list (* r (cos phi) (cos th)) (* r (sin phi)) (* r (cos phi) (sin th)))))
  (define (vidx i j) (+ (* i (+ lon 1)) j))
  (define (ring-verts i j acc)
    (if (> j lon) acc (ring-verts i (+ j 1) (append acc (vert i j)))))
  (define (all-verts i acc)
    (if (> i lat) acc (all-verts (+ i 1) (ring-verts i 0 acc))))
  (define (row-quads i j acc)
    (if (>= j lon) acc
        (row-quads i (+ j 1)
                   (cons (list (vidx i j) (vidx (+ i 1) j)
                               (vidx (+ i 1) (+ j 1)) (vidx i (+ j 1))) acc))))
  (define (all-quads i acc)
    (if (>= i lat) acc (all-quads (+ i 1) (row-quads i 0 acc))))
  (mesh-make (all-verts 0 '()) (mesh-quads-idx (all-quads 0 '()))))

;; тор: R — радиус кольца, r — трубки, ring x seg сегментов
(define (mesh-torus R r ring seg)
  (define (vert i j)
    (let* ((u (* 6.28318530 (/ i ring)))
           (w (* 6.28318530 (/ j seg)))
           (cx (+ R (* r (cos w)))))
      (list (* cx (cos u)) (* r (sin w)) (* cx (sin u)))))
  (define (vidx i j) (+ (* i (+ seg 1)) j))
  (define (ring-verts i j acc)
    (if (> j seg) acc (ring-verts i (+ j 1) (append acc (vert i j)))))
  (define (all-verts i acc)
    (if (> i ring) acc (all-verts (+ i 1) (ring-verts i 0 acc))))
  (define (row-quads i j acc)
    (if (>= j seg) acc
        (row-quads i (+ j 1)
                   (cons (list (vidx i j) (vidx (+ i 1) j)
                               (vidx (+ i 1) (+ j 1)) (vidx i (+ j 1))) acc))))
  (define (all-quads i acc)
    (if (>= i ring) acc (all-quads (+ i 1) (row-quads i 0 acc))))
  (mesh-make (all-verts 0 '()) (mesh-quads-idx (all-quads 0 '()))))

;; плоскость y=0 размера 2a, subdiv ячеек на сторону
(define (mesh-plane a subdiv)
  (define (vert i j)
    (list (- (* 2 a (/ i subdiv)) a) 0 (- (* 2 a (/ j subdiv)) a)))
  (define (vidx i j) (+ (* i (+ subdiv 1)) j))
  (define (row-verts i j acc)
    (if (> j subdiv) acc (row-verts i (+ j 1) (append acc (vert i j)))))
  (define (all-verts i acc)
    (if (> i subdiv) acc (all-verts (+ i 1) (row-verts i 0 acc))))
  (define (row-quads i j acc)
    (if (>= j subdiv) acc
        (row-quads i (+ j 1)
                   (cons (list (vidx i j) (vidx i (+ j 1))
                               (vidx (+ i 1) (+ j 1)) (vidx (+ i 1) j)) acc))))
  (define (all-quads i acc)
    (if (>= i subdiv) acc (all-quads (+ i 1) (row-quads i 0 acc))))
  (mesh-make (all-verts 0 '()) (mesh-quads-idx (all-quads 0 '()))))

;; линии сетки плоскости (пары индексов для gpu3d-wire)
(define (mesh-plane-lines subdiv)
  (define (vidx i j) (+ (* i (+ subdiv 1)) j))
  (define (row-lines i j acc)
    (if (>= j subdiv) acc
        (row-lines i (+ j 1) (append acc (list (vidx i j) (vidx i (+ j 1)))))))
  (define (col-lines j i acc)
    (if (>= i subdiv) acc
        (col-lines j (+ i 1) (append acc (list (vidx i j) (vidx (+ i 1) j))))))
  (define (go-rows i acc)
    (if (> i subdiv) acc (go-rows (+ i 1) (row-lines i 0 acc))))
  (define (go-cols j acc)
    (if (> j subdiv) acc (go-cols (+ j 1) (col-lines j 0 acc))))
  (f32-from-list (append (go-rows 0 '()) (go-cols 0 '()))))

;; рёбра треугольной сетки парами (каркасный режим gpu3d-wire)
(define (mesh-edges idx)
  (if (< (length idx) 3) '()
      (append (list (car idx) (cadr idx)
                    (cadr idx) (caddr idx)
                    (caddr idx) (car idx))
              (mesh-edges (cdddr idx)))))

;; ——— отрисовка ———
;; Этап 7: draw-хелперы принимают ОПЦИОНАЛЬНОЕ имя материала
;; (последним аргументом): (draw-mesh mesh model mvp color [материал]) —
;; применяется (use-material! имя). Все прежние вызовы (без материала)
;; работают как раньше. Материалы: assets/lisp/materials.lisp.
(define (draw-mesh mesh model mvp color . opt)
  (if (null? opt) #f (use-material! (car opt)))
  (gpu3d-mesh (mesh-pos mesh) (mesh-idx mesh) model mvp
              (car color) (cadr color) (caddr color)))

;; ——— БАТЧ-ВЕРШИННИК (v0.3): много объектов одним draw call ———
;; Ряд инстанса — 12 f32: tx ty tz sx sy sz yaw pitch roll r g b.
;; Трансформы и цвета собираются на Lisp, T&L и растр — одним вызовом «GPU».
(define (inst-row x y z s yaw r g b)
  (list x y z s s s yaw 0.0 0.0 r g b))

(define (inst-row-rpy x y z s yaw pitch roll r g b)
  (list x y z s s s yaw pitch roll r g b))

(define (draw-batch mesh insts mvp . opt)
  (if (null? opt) #f (use-material! (car opt)))
  (gpu3d-batch (mesh-pos mesh) (mesh-idx mesh) (f32-from-list insts) mvp))

;; материал переопределяет текстуру: :texture из :texture-слота или tex-аргумент
(define (draw-batch-tex mesh uv tex insts mvp . opt)
  (if (null? opt) #f (use-material! (car opt)))
  (gpu3d-batch-tex (mesh-pos mesh) uv (mesh-idx mesh)
                   (if (null? opt) tex (or (material-texture (car opt)) tex))
                   (f32-from-list insts) mvp))


;; wire-объект: (f32-позиции f32-пары-индексов)
(define (wire-make pos idx)
  (list (f32-from-list pos) (f32-from-list idx)))
(define (wire-pos w) (car w))
(define (wire-idx w) (cadr w))

(define (draw-wire-lines wire model mvp color)
  (gpu3d-wire (wire-pos wire) (wire-idx wire) model mvp
              (car color) (cadr color) (caddr color)))

(define (mesh-to-wire mesh)
  (wire-make (f32vec->list (mesh-pos mesh)) (mesh-edges (f32vec->list (mesh-idx mesh)))))
