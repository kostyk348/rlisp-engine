;; textures.lisp — Уровень 3: Texture DSL (v0.3).
;; Поверх HAL-нативов texture-upload/load/gpu-blit-tex:
;;   — процедурные текстуры (canvas на F32Vec, попиксельный Lisp-цикл,
;;     горячая часть только при создании — как в Unity при импорте ассета);
;;   — загрузка PNG с диска (свой декодер в png.rs, без зависимостей);
;;   — UV-развёртки примитивов render3d (куб/плоскость/сфера/тор);
;;   — спрайты 2D и текстурные draw-обёртки.
;;
;; Идентификаторы: id = 1,2,3…; 0 = «без текстуры» (как в OpenGL).

;; ——— canvas: буфер RGBA на F32Vec (каналы 0..1) ———
(define (tex-canvas w h) (make-f32vec (* w h 4) 0.0))

(define (tex-px! buf w x y r g b)
  (let ((i (* 4 (+ (* y w) x))))
    (f32set! buf i r)
    (f32set! buf (+ i 1) g)
    (f32set! buf (+ i 2) b)
    (f32set! buf (+ i 3) 1.0)))

(define (tex-fin buf w h) (texture-upload w h buf))

;; ——— загрузка PNG с диска ———
(define (load-texture path) (texture-load path))

;; ——— процедурные генераторы ———

;; шахматка: n×n, клетка cell, цвета c1/c2
(define (tex-checker n cell c1 c2)
  (let ((buf (tex-canvas n n)))
    (dotimes (y n)
      (dotimes (x n)
        (let ((c (if (= (mod (+ (floor (/ x cell)) (floor (/ y cell))) 2) 0) c1 c2)))
          (tex-px! buf n x y (car c) (cadr c) (caddr c)))))
    (tex-fin buf n n)))

;; неоновая схема «пол арены»: тёмный фон, светящиеся линии сетки,
;; узлы-перекрёстки, лёгкий шум — единый текстурный draw вместо wire-циклов
(define (tex-circuit n cell)
  (let ((buf (tex-canvas n n)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((gx (mod x cell))
               (gy (mod y cell))
               (node? (and (< gx 3) (< gy 3)))
               (line? (or (< gx 1) (< gy 1)))
               (spark (< (random 100) 2))
               (base (+ 0.045 (* 0.02 (/ (random 100) 100.0)))))
          (if node?
              (tex-px! buf n x y 0.20 1.00 1.10)
              (if line?
                  (tex-px! buf n x y 0.10 0.62 0.78)
                  (if spark
                      (tex-px! buf n x y 0.16 0.45 0.52)
                      (tex-px! buf n x y (* 1.9 base) (+ (* 2.1 base) 0.02) (+ (* 2.4 base) 0.09))))))))
    (tex-fin buf n n)))

;; «панель» для врагов: белая кромка (тинт даёт цвет), тёмное нутро,
;; диагональная штриховка — выглядит как броня
(define (tex-panel n)
  (let ((buf (tex-canvas n n)))
    (dotimes (y n)
      (dotimes (x n)
        (let ((edge (or (< x 3) (< y 3) (>= x (- n 3)) (>= y (- n 3))))
              (stripe (< (mod (+ x y) 14) 2)))
          (if edge
              (tex-px! buf n x y 1.0 1.0 1.0)
              (if stripe
                  (tex-px! buf n x y 0.85 0.85 0.9)
                  (tex-px! buf n x y 0.42 0.42 0.48))))))
    (tex-fin buf n n)))

;; шум (звёздное небо, помехи)
(define (tex-noise n)
  (let ((buf (tex-canvas n n)))
    (dotimes (y n)
      (dotimes (x n)
        (let ((v (/ (random 100) 100.0)))
          (tex-px! buf n x y v v v))))
    (tex-fin buf n n)))

;; вертикальный градиент (небо одним quad'ом)
(define (tex-gradient n c1 c2)
  (let ((buf (tex-canvas n n)))
    (dotimes (y n)
      (let ((k (/ y (- n 1.0))))
        (dotimes (x n)
          (tex-px! buf n x y
                   (+ (car c1) (* (- (car c2) (car c1)) k))
                   (+ (cadr c1) (* (- (cadr c2) (cadr c1)) k))
                   (+ (caddr c1) (* (- (caddr c2) (caddr c1)) k))))))
    (tex-fin buf n n)))

;; ——— UV-развёртки примитивов (пары u v на вершину) ———

;; куб из mesh-cube: 6 граней × 4 вершины, каждая грань — полный 0..1
(define (mesh-cube-uv)
  (f32-from-list
   (append (uv-quad) (uv-quad) (uv-quad) (uv-quad) (uv-quad) (uv-quad))))
(define (uv-quad) (list 0 0  1 0  1 1  0 1))

;; плоскость subdiv×subdiv
(define (mesh-plane-uv subdiv)
  (define (row i j acc)
    (if (> j subdiv) acc
        (row i (+ j 1) (append acc (list (/ i subdiv) (/ j subdiv))))))
  (define (all i acc)
    (if (> i subdiv) acc (all (+ i 1) (row i 0 acc))))
  (f32-from-list (all 0 '())))

;; UV-сфера
(define (mesh-sphere-uv lat lon)
  (define (row i j acc)
    (if (> j lon) acc
        (row i (+ j 1) (append acc (list (/ j lon) (/ i lat))))))
  (define (all i acc)
    (if (> i lat) acc (all (+ i 1) (row i 0 acc))))
  (f32-from-list (all 0 '())))

;; тор
(define (mesh-torus-uv ring seg)
  (define (row i j acc)
    (if (> j seg) acc
        (row i (+ j 1) (append acc (list (/ i ring) (/ j seg))))))
  (define (all i acc)
    (if (> i ring) acc (all (+ i 1) (row i 0 acc))))
  (f32-from-list (all 0 '())))

;; ——— отрисовка ———

;; 2D-спрайт: (draw-sprite tex x y [scale [tint]]) — прозрачность из альфы
(define (draw-sprite tex x y . opt)
  (gpu-blit-tex tex x y
                (if (null? opt) 1.0 (car opt))
                (if (>= (length opt) 2) (cadr opt) (list 1 1 1))))

;; текстурный mesh одним draw call.
;; Этап 7: опциональное имя материала последним аргументом — применяется
;; (use-material! имя), текстура материала (:texture) переопределяет tex.
(define (draw-mesh-tex mesh uv tex model mvp tint . opt)
  (if (null? opt) #f (use-material! (car opt)))
  (gpu3d-mesh-tex (mesh-pos mesh) uv (mesh-idx mesh)
                  (if (null? opt) tex (or (material-texture (car opt)) tex))
                  model mvp
                  (car tint) (cadr tint) (caddr tint)))
