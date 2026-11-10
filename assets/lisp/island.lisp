;; island.lisp — Уровень 4: сцена «ОСТРОВ» (этап 8, визуальная ревизия 2).
;; Демонстрация: материалы (этап 7) + ВОДА С ВОЛНАМИ + деревья (alpha-test
;; кроны) + камни + свет/туман + glTF-модели из интернета (load-gltf.lisp).
;;
;; Ревизия 2 (после честного разбора первого рендера):
;;   1. (gpu3d-clear …) в начале кадра — без него z-буфер предыдущего кадра
;;      «съедал» следующий (остров пропадал за мусорной глубиной);
;;   2. glTF-модели нормализуются по bbox (у Lantern реальные 5 юнитов —
;;      фиксированный ×7 делал из него 35-юнитового монстра у камеры);
;;   3. три полосы ландшафта по высоте (песок/трава/камень) с дизерингом
;;      границы — один вершинный буфер, три индексных;
;;   4. кроны деревьев — 3 слоя скрещенных квадрантов, пятнистая листва
;;      вместо «шахматной», разброс оттенков по дереву;
;;   5. тёплый свет, камера ближе/ниже, туман в цвет неба.
;;
;; Текстуры процедурные и ДЕТЕРМИНИРОВАНЫ (без (random)); сцена повторяема.
;; glTF-модели — KhronosGroup/glTF-Sample-Assets, CC0-1.0 проверен по
;; LICENSE.md каждой модели (копия в assets/models/<name>/LICENSE.md).

(window-create *screen-w* *screen-h* "ISLAND :: rlisp-engine 3D")

;; ——— persistent-состояние: переживает hot reload ———
(defpersistent *t* 0.0)
(defpersistent *frame* 0)
(defpersistent *fps-last* 0.0)
(defpersistent *fps-last-val* 0.0)

;; ——— мир ———
(define *ia* 20.0)            ; полуразмер острова
(define *in* 24)              ; клеток острова (25×25 вершин)
(define *wa* 30.0)            ; полуразмер водного плана
(define *wn* 20)              ; клеток воды (21×21 вершин)

;; детерминированный «шум» [0..1) — без (random) и без mod на float
(define (hash2 x y)
  (abs (sin (+ (* x 12.9898) (* y 78.233)))))

;; высота острова: параболический купол + синусный шум; низкий берег —
;; широкая пляжная полоса; под водой дно до -2
(define (isl-h x z)
  (let* ((r (sqrt (+ (* x x) (* z z))))
         (q (max 0.0 (- 1.0 (/ (* r r) 240.0))))
         (dome (* 3.4 q q))
         (n (+ (* 0.42 (sin (+ (* x 0.55) (* z 0.95))))
               (* 0.28 (cos (+ (* x 1.3) (* z -0.6))))
               (* 0.15 (sin (+ (* x 2.4) (* z 1.9))))
               (* 0.10 (sin (* x 4.1)))
               (* 0.10 (cos (* z 3.7)))))
         (h0 (+ dome n))
         (h1 (max -2.0 (if (< h0 0.6) (+ 0.6 (* 1.7 (- h0 0.6))) h0))))
    ;; за радиусом 19 — только дно (иначе шум рождает песчаные островки)
    (if (> r 19.0)
        (min h1 (- -0.25 (* 0.25 (- r 19.0))))
        h1)))

;; волна: сумма трёх синусов (детерминирована, параметр — время)
(define (wave-y x z t)
  (+ (* 0.18 (sin (+ (* 1.1 x) (* 2.0 t))))
     (* 0.12 (sin (+ (* 1.4 z) (* -1.6 t))))
     (* 0.07 (sin (+ (* 2.3 (+ x z)) (* 3.1 t))))))

;; ——— сетка-хелпер: вершины/UV/индексы плоскости XZ ———
(define (grid-make a n hfn)
  (let ((v (make-f32vec (* (+ n 1) (+ n 1) 3) 0.0))
        (uv (make-f32vec (* (+ n 1) (+ n 1) 2) 0.0))
        (idx '()))
    (define (vx i) (- (* 2 a (/ i n)) a))
    (define (vz j) (- (* 2 a (/ j n)) a))
    (define (vi i j) (+ (* i (+ n 1)) j))
    (define (go-cols i j)
      (if (>= j n)
          #f
          (begin
            (set! idx (append idx (list (vi i j) (vi i (+ j 1))
                                        (vi (+ i 1) (+ j 1))
                                        (vi i j) (vi (+ i 1) (+ j 1))
                                        (vi (+ i 1) j))))
            (go-cols i (+ j 1)))))
    (define (go-rows i)
      (if (>= i n)
          #f
          (begin (go-cols i 0) (go-rows (+ i 1)))))
    (dotimes (i (+ n 1))
      (dotimes (j (+ n 1))
        (let ((k (* (+ (* i (+ n 1)) j) 3))
              (u (* (+ (* i (+ n 1)) j) 2)))
          (f32set! v k (vx i))
          (f32set! v (+ k 1) (hfn (vx i) (vz j)))
          (f32set! v (+ k 2) (vz j))
          (f32set! uv u (/ i n))
          (f32set! uv (+ u 1) (/ j n)))))
    (go-rows 0)
    (list v uv (f32-from-list idx))))

;; ——— остров: один вершинный буфер, три полосы по высоте ———
(define *island* (grid-make *ia* *in* isl-h))
(define *ipos* (car *island*))
(define *iuv* (cadr *island*))

;; граница полос дизерируется синусом, чтобы не была «ласточкиной»
(define (band-jitter x z)
  (+ (* 0.05 (sin (+ (* x 0.9) (* z 1.4))))
     (* 0.04 (sin (+ (* x 1.7) (* z -0.8))))))

(define (cell-band x z)
  (let* ((h (isl-h x z))
         (j (band-jitter x z))
         (r (sqrt (+ (* x x) (* z z))))
         ;; берег — РАДИАЛЬНОЕ кольцо с дизером кромки (по высоте шум
         ;; рвал пляж на лоскуты)
         (rb (+ 12.5 (* 1.2 (sin (+ (* x 0.8) (* z 0.6)))))))
    (cond ((> r rb) 0)                    ; песок — кольцо у воды
          ((> h (+ 2.70 j)) 2)            ; камень — только вершина
          (else 1))))                     ; трава

;; три индексных набора по средней высоте 4 углов ячейки
(define (grid-split-idx)
  (define (vi i j) (+ (* i (+ *in* 1)) j))
  (define (vx i) (- (* 2 *ia* (/ i *in*)) *ia*))
  (define (vz j) (- (* 2 *ia* (/ j *in*)) *ia*))
  (let ((sand '()) (grass '()) (rock '()))
    (define (go-cols i j)
      (if (>= j *in*)
          #f
          (let* ((b (cell-band (/ (+ (vx i) (vx (+ i 1))) 2.0)
                               (/ (+ (vz j) (vz (+ j 1))) 2.0)))
                 (q (list (vi i j) (vi i (+ j 1)) (vi (+ i 1) (+ j 1))
                          (vi i j) (vi (+ i 1) (+ j 1)) (vi (+ i 1) j))))
            (cond ((= b 0) (set! sand (append sand q)))
                  ((= b 1) (set! grass (append grass q)))
                  (else (set! rock (append rock q))))
            (go-cols i (+ j 1)))))
    (define (go-rows i)
      (if (>= i *in*)
          #f
          (begin (go-cols i 0) (go-rows (+ i 1)))))
    (go-rows 0)
    (list (f32-from-list sand)
          (f32-from-list grass)
          (f32-from-list rock))))

(define *idx-sand* (car (grid-split-idx)))
(define *idx-grass* (cadr (grid-split-idx)))
(define *idx-rock* (caddr (grid-split-idx)))

;; ——— вода ———
(define *water-raw* (grid-make *wa* *wn* (lambda (x z) 0.0)))
(define *wpos* (car *water-raw*))
(define *wuv* (cadr *water-raw*))
(define *widx* (caddr *water-raw*))

(define (update-water!)
  (define (vx i) (- (* 2 *wa* (/ i *wn*)) *wa*))
  (define (vz j) (- (* 2 *wa* (/ j *wn*)) *wa*))
  (dotimes (i (+ *wn* 1))
    (dotimes (j (+ *wn* 1))
      (f32set! *wpos* (+ (* (+ (* i (+ *wn* 1)) j) 3) 1)
               (wave-y (vx i) (vz j) *t*))))
  #t)

;; ——— процедурные текстуры (canvas на F32Vec, 64×64) ———
(define (isl-px! b w x y r g bb)
  (let ((i (* 4 (+ (* y w) x))))
    (f32set! b i r) (f32set! b (+ i 1) g)
    (f32set! b (+ i 2) bb) (f32set! b (+ i 3) 1.0)))
(define (isl-pxa! b w x y r g bb a)
  (let ((i (* 4 (+ (* y w) x))))
    (f32set! b i r) (f32set! b (+ i 1) g)
    (f32set! b (+ i 2) bb) (f32set! b (+ i 3) a)))

;; трава: зернистая, два тона, без крупных «плит» (тайлинг 4 читался
;; как шахматка — частоту пятен подняли, крошки больше)
(define (isl-tex-grass n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((bl (+ (sin (+ (* x 1.3) (* y 2.6)))
                      (* 0.6 (cos (+ (* x 3.1) (* y -1.5))))))
               (sp (hash2 x y))
               (sp2 (hash2 (+ x 17) (+ y 41)))
               (g (clamp (+ 0.31 (* 0.04 bl) (* 0.15 sp) (* 0.10 sp2))
                         0.14 0.58)))
          (isl-px! b n x y (* 0.48 g) g (* 0.36 g)))))
    (texture-upload n n b)))

;; песок: тёплый светлый + рябь и крошка
(define (isl-tex-sand n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((sp (hash2 (+ x 7) y))
               (rip (* 0.05 (sin (+ (* y 0.9) (* x 0.2)))))
               (v (clamp (+ 0.78 rip (* 0.10 sp)) 0.6 0.95)))
          (isl-px! b n x y v (* 0.91 v) (* 0.72 v)))))
    (texture-upload n n b)))

;; вода: светлая бирюза, широкие мягкие полосы (блик даёт материал)
(define (isl-tex-water n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((v (clamp (+ 0.68 (* 0.08 (sin (+ (* x 0.5) (* y 2.2))))
                            (* 0.05 (cos (+ (* x 2.8) (* y -0.9)))))
                         0.45 0.85)))
          (isl-px! b n x y (* 0.20 v) (* 0.58 v) (* 0.90 v)))))
    (texture-upload n n b)))

;; кора: вертикальные борозды по коричневому
(define (isl-tex-bark n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let ((k (clamp (- 0.55 (* 0.22 (sin (* x 1.8)) (sin (* y 0.35))))
                        0.15 0.75)))
          (isl-px! b n x y (* 1.15 k) (* 0.8 k) (* 0.55 k)))))
    (texture-upload n n b)))

;; листва: крупные пятна с дырками (alpha=0) под alpha-test 0.5
(define (isl-tex-leaves n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((blob (+ (sin (+ (* x 0.5) (* y 0.8)))
                        (* 0.7 (cos (+ (* x 1.1) (* y -0.6))))
                        (* 0.5 (sin (+ (* x 2.1) (* y 1.7))))))
               (sp (hash2 (+ x 3) (+ y 9)))
               (hole (< blob -1.15))
               (g (clamp (+ 0.30 (* 0.20 (+ 0.5 (* 0.5 blob))) (* 0.10 sp))
                         0.12 0.6)))
          (if hole
              (isl-pxa! b n x y 0 0 0 0.0)
              (isl-pxa! b n x y (* 0.44 g) g (* 0.30 g) 1.0)))))
    (texture-upload n n b)))

;; камень: серый с трещинами
(define (isl-tex-rock n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((k (+ 0.52 (* 0.18 (sin (+ (* x 1.1) (* y 0.7))))
                     (* 0.12 (cos (+ (* x 2.9) (* y -1.8))))))
               (crack (< (abs (sin (+ (* x 0.4) (* y 2.9)))) 0.05))
               (v (if crack (* 0.55 k) k)))
          (isl-px! b n x y (* 1.04 v) (* 0.94 v) (* 0.84 v)))))
    (texture-upload n n b)))

;; ——— материалы (этап 7) ———
(defmaterial 'песок ':tiling '(4 4))
(defmaterial 'трава ':tiling '(3 3))
(defmaterial 'камень ':tiling '(2 2) ':spec 0.12 ':shininess 24)
(defmaterial 'кора ':tiling '(2 3))
(defmaterial 'листва ':alpha-test 0.5)
(defmaterial 'вода ':tiling '(2 2) ':spec 0.7 ':shininess 50)
(defmaterial 'глинт ':spec 0.4 ':shininess 40)

;; ——— деревья: 3 кластера, ствол-батч + крона из 3 слоёв скрещенных
;; квадрантов (двусторонние), оттенок кроны свой у каждого дерева ———
(define *tree-n* 18)
(define *cluster-xy* (list (list 5.5 -3.5) (list -6.0 2.5) (list 1.5 6.5)))

(define (tree-pos k)
    (let* ((c (nth *cluster-xy* (mod k 3)))
         (cx (car c)) (cz (cadr c))
         (ang (+ (* k 2.39996323) (* (mod k 3) 2.1)))
         (rad (+ 0.6 (* 2.6 (hash2 k 13))))
         (x (+ cx (* rad (cos ang))))
         (z (+ cz (* rad (sin ang)))))
    (list x (isl-h x z) z)))

(define (tree-scale k) (+ 0.8 (* 0.6 (hash2 k 5))))

(define *trunk-mesh* (mesh-cube 1.0))
(define *trunk-uv* (mesh-cube-uv))

(define (trunk-insts)
  (define (go k acc)
    (if (>= k *tree-n*)
        acc
        (let* ((p (tree-pos k))
               (s (tree-scale k)))
          (go (+ k 1)
              (append acc
                      (list (car p) (+ (cadr p) (* 0.7 s)) (caddr p)
                            (* 0.13 s) (* 1.5 s) (* 0.13 s)
                            (* k 1.7) 0 0 0.8 0.68 0.55))))))
  (go 0 '()))

;; крона: 3 слоя × 2 скрещенных квадрата, оба бока (24 треугольника/дерево)
(define (crown-geo)
  (let ((pos '()) (uv '()) (idx '()) (vi 0))
    (define (hw L) (nth (list 1.35 1.1 0.75) L))
    (define (y0 L) (nth (list 0.0 0.95 1.7) L))
    (define (y1 L) (+ (y0 L) (nth (list 1.6 1.35 1.05) L)))
    (define (quad ax ay az bx by bz cx cy cz dx dy dz)
      ;; 4 вершины, uv (0 0)(1 0)(1 1)(0 1), оба бока
      (set! pos (append pos (list ax ay az bx by bz cx cy cz dx dy dz)))
      (set! uv (append uv (list 0 0 1 0 1 1 0 1)))
      (set! idx (append idx (list vi (+ vi 1) (+ vi 2)  vi (+ vi 2) (+ vi 3)
                                  (+ vi 2) (+ vi 1) vi  (+ vi 3) (+ vi 2) vi)))
      (set! vi (+ vi 4)))
    (dotimes (L 3)
      (let ((w (hw L)) (a (y0 L)) (bb (y1 L)))
        ;; вдоль X (нормаль по Z)
        (quad (- w) a 0  w a 0  w bb 0  (- w) bb 0)
        ;; вдоль Z (нормаль по X)
        (quad 0 a (- w)  0 a w  0 bb w  0 bb (- w))))
    (list (f32-from-list pos) (f32-from-list uv) (f32-from-list idx))))

(define *crown* (crown-geo))

(define (crown-insts)
  (define (go k acc)
    (if (>= k *tree-n*)
        acc
        (let* ((p (tree-pos k))
               (s (tree-scale k))
               (sh (+ 0.85 (* 0.3 (hash2 k 23)))))
          (go (+ k 1)
              (append acc
                      (list (car p) (+ (cadr p) (* 1.55 s)) (caddr p)
                            s s s (* k 1.7) 0 0
                            (* 0.9 sh) sh (* 0.75 sh)))))))
  (go 0 '()))

;; камни: октаэдры (батч), чуть утоплены
(define *rock-pos*
  (f32-from-list (list 1 0 0  -1 0 0  0 1 0  0 -1 0  0 0 1  0 0 -1)))
(define *rock-idx*
  (f32-from-list (list 0 2 4  2 1 4  1 3 4  3 0 4
                       2 0 5  1 2 5  3 1 5  0 3 5)))
(define *rock-uv*
  (f32-from-list (list 0.7 0.5  0.3 0.5  0.5 0.85
                       0.5 0.15  0.85 0.5  0.15 0.5)))
(define (rock-rows)
  (define (go k acc)
    (if (>= k 10)
        acc
        (let* ((ang (+ 0.4 (* k 2.7)))
               (rad (+ 4.5 (* 5.5 (hash2 k 31))))
               (x (* rad (cos ang)))
               (z (* rad (sin ang)))
               (s (+ 0.55 (* 0.75 (hash2 k 7)))))
          (go (+ k 1)
              (append acc
                      (list x (- (isl-h x z) (* 0.25 s)) z
                            (* 1.4 s) (* 0.7 s) (* 1.2 s)
                            (* k 2.1) 0.1 0 1 1 1))))))
  (go 0 '()))

;; ——— glTF-модели: НОРМАЛИЗАЦИЯ ПО BBOX (модели из интернета приходят
;; в любых единицах — Lantern в 5 юнитов, бутылка в 0.26). Выбросы >4×
;; минимума (служебные меши «света» у Lantern — 25 юнитов) не учитываем.
(define (gltf-extents ms)
  (define (mesh-extent m)
    (define (go i lo hi v n)
      (if (>= i n)
          (list lo hi)
          (go (+ i 3)
              (list (min (car lo) (f32ref v i))
                    (min (cadr lo) (f32ref v (+ i 1)))
                    (min (caddr lo) (f32ref v (+ i 2))))
              (list (max (car hi) (f32ref v i))
                    (max (cadr hi) (f32ref v (+ i 1)))
                    (max (caddr hi) (f32ref v (+ i 2))))
              v n)))
    (let* ((v (table-get m 'pos))
           (r (go 0 (list 1e9 1e9 1e9) (list -1e9 -1e9 -1e9) v (f32vec-length v))))
      (max (max (- (car (cadr r)) (car (car r)))
                (- (cadr (cadr r)) (cadr (car r))))
           (- (caddr (cadr r)) (caddr (car r))))))
  (define (go ms acc)
    (if (null? ms) acc (go (cdr ms) (append acc (list (mesh-extent (car ms)))))))
  (go ms '()))

(define (gltf-normalize-extent ms)
  (define (maxle lst cap acc)
    (if (null? lst)
        acc
        (maxle (cdr lst) cap
               (if (and (> (car lst) acc) (<= (car lst) cap))
                   (car lst)
                   acc))))
  (define (minof lst acc)
    (if (null? lst) acc (minof (cdr lst) (min acc (car lst)))))
  (let* ((es (gltf-extents ms))
         (lo (minof es 1e9))
         (e (maxle es (* 4.0 lo) 0.0)))
    (if (> e 0.0001) e 1.0)))

(define (gltf-scale-for ms target)
  (let ((e (gltf-normalize-extent ms)))
    (/ target e)))

;; служебные меши-выбросы (>4× минимума, «свет» Lantern) — не рисуем:
;; их геометрия в 25 юнитов даёт гигантские плиты у камеры
(define (gltf-filter-outliers ms)
  (let* ((es (gltf-extents ms)))
    (define (minof lst acc)
      (if (null? lst) acc (minof (cdr lst) (min acc (car lst)))))
    (define (go lst es acc)
      (if (null? lst)
          acc
          (go (cdr lst) (cdr es)
              (if (<= (car es) (* 4.0 (minof es 1e9)))
                  (append acc (list (car lst)))
                  acc))))
    (go ms es '())))

(define *m-bottle*
  (if (file-exists? "assets/models/waterbottle/WaterBottle.gltf")
      (gltf-load "assets/models/waterbottle/WaterBottle.gltf")
      '()))
(define *m-lantern*
  (if (file-exists? "assets/models/lantern/Lantern.gltf")
      (gltf-load "assets/models/lantern/Lantern.gltf")
      '()))
(define *s-bottle* (gltf-scale-for *m-bottle* 1.15))
(define *s-lantern* (gltf-scale-for *m-lantern* 1.7))
(define *m-bottle-draw* (gltf-filter-outliers *m-bottle*))
(define *m-lantern-draw* (gltf-filter-outliers *m-lantern*))

(define (model-mat x y z s yaw)
  (mat4-mul (mat-translate x y z)
            (mat4-mul (mat-rotate-y yaw) (mat-scale s s s))))

;; ——— сцена ———
(define (island-tris)
  (+ (/ (f32vec-length *idx-sand*) 3)
     (/ (f32vec-length *idx-grass*) 3)
     (/ (f32vec-length *idx-rock*) 3)
     (/ (f32vec-length *widx*) 3)
     (* *tree-n* 40)
     40))

;; небо: 16 полос от дымки у горизонта к зениту (2D, до 3D)
(define (draw-sky)
  (define (go i y)
    (if (>= i 16)
        #f
        (let ((k (/ i 15.0)))
          (draw-rect 0 y *screen-w* 15
                     (list (- 0.80 (* 0.38 k))
                           (- 0.86 (* 0.26 k))
                           (- 0.93 (* 0.07 k))))
          (go (+ i 1) (+ y 15)))))
  (go 0 0))

(define (draw-scene)
  (let* ((a (* *t* 0.10))
         (eye (list (* 21.5 (cos a)) 10.0 (* 21.5 (sin a))))
         (mvp (camera-mvp eye (list 0 1.3 0) 0.95)))
    ;; цвет = низ неба; ЧИСТИТ И Z-БУФЕР — без этого кадр ест предыдущий
    (gpu3d-clear 0.66 0.78 0.90)
    (gpu3d-view (car eye) (cadr eye) (caddr eye))
    (gpu3d-light 0.45 0.72 0.32 0.36 0.85 0.5 48)
    (gpu3d-fog 0.66 0.78 0.90 0.010)
    ;; ландшафт: три полосы одним вершинным буфером
    (draw-mesh-tex (list *ipos* *idx-sand*) *iuv* *tex-sand*
                   (mat-identity) mvp (list 1 1 1) 'песок)
    (draw-mesh-tex (list *ipos* *idx-grass*) *iuv* *tex-grass*
                   (mat-identity) mvp (list 1 1 1) 'трава)
    (draw-mesh-tex (list *ipos* *idx-rock*) *iuv* *tex-rock*
                   (mat-identity) mvp (list 1 1 1) 'камень)
    ;; деревья: стволы + кроны (alpha-test), камни
    (draw-batch-tex *trunk-mesh* *trunk-uv* *tex-bark*
                    (trunk-insts) mvp 'кора)
    (draw-batch-tex (list (car *crown*) (caddr *crown*)) (cadr *crown*)
                    *tex-leaves* (crown-insts) mvp 'листва)
    (draw-batch-tex (list *rock-pos* *rock-idx*) *rock-uv* *tex-rock*
                    (rock-rows) mvp 'камень)
    ;; glTF-модели, нормализованные по bbox
    (if (null? *m-bottle-draw*)
        #f
        (draw-gltf *m-bottle-draw*
                   (model-mat 5.2 (+ (isl-h 5.2 3.0) 0.05) 3.0 *s-bottle* 0.7)
                   mvp (list 1 1 1)))
    (if (null? *m-lantern-draw*)
        #f
        (draw-gltf *m-lantern-draw*
                   (model-mat -4.6 (+ (isl-h -4.6 4.4) 0.05) 4.4 *s-lantern* -0.5)
                   mvp (list 1 1 1) 'глинт))
    ;; вода — последней; солнечный блик через материал 'вода
    (use-material! 'вода)
    (gpu3d-mesh-tex *wpos* *wuv* *widx* *tex-water* (mat-identity) mvp 1 1 1)
    (material-reset!)))

(define (draw-hud)
  (draw-text "ISLAND" 6 5 1 (list 1.0 1.0 1.0))
  (draw-text (string-append "TRIS " (number->string (island-tris)))
             6 13 1 (list 0.6 0.7 0.85))
  (draw-text (string-append "FPS " (number->string *fps-last-val*))
             6 21 1 (list 0.7 0.9 0.6)))

(define (draw)
  (draw-sky)
  (draw-scene)
  (draw-hud)
  (gpu-present))

(define (update dt)
  (set! *t* (+ *t* dt))
  (set! *frame* (+ *frame* 1))
  ;; волны — каждый 2-й кадр (честный компромисс, см. LIMITATIONS.md)
  (if (= 0 (mod *frame* 2))
      (update-water!)
      #f)
  ;; честный fps: раз в 120 кадров — реальная скорость headless-рендера
  (if (>= *frame* (+ *fps-last* 120.0))
      (begin
        (set! *fps-last-val*
              (/ 120.0 (max 0.001 (- (time-now) *fps-clock*))))
        (set! *fps-clock* (time-now))
        (set! *fps-last* *frame*)
        (print (string-append "ISLAND fps: "
                              (number->string *fps-last-val*))))
      #f)
  #t)

;; ——— текстуры и часы (после всех определений) ———
(define *tex-grass* (isl-tex-grass 64))
(define *tex-sand* (isl-tex-sand 64))
(define *tex-water* (isl-tex-water 64))
(define *tex-bark* (isl-tex-bark 64))
(define *tex-leaves* (isl-tex-leaves 64))
(define *tex-rock* (isl-tex-rock 64))
(define *fps-clock* (time-now))
(print "ISLAND: остров(песок/трава/камень)/вода/деревья/камни/glTF готовы; "
       "масштаб моделей: bottle " *s-bottle* " lantern " *s-lantern*)
