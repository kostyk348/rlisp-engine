;; island.lisp — Уровень 4: сцена «ОСТРОВ» (этап 8).
;; Демонстрация: материалы (этап 7) + ВОДА С ВОЛНАМИ + деревья (alpha-test
;; кроны) + камни + свет/туман + glTF-модели из интернета (load-gltf.lisp).
;; Всё процедурно и ДЕТЕРМИНИРОВАНО (без (random)) — захваты повторяемы.
;;
;; Текстуры: процедурные (как в ORBITAL v0.3). ambientcg.com из песочницы
;; сборки недоступен (TCP-connect висит), лицензию зеркала подтвердить не
;; удалось — движок рисует их сам (см. docs/credits.md): никаких
;; непроверенных CC0-претензий. glTF-модели — KhronosGroup/
;; glTF-Sample-Assets, CC0-1.0 проверен по LICENSE.md каждой модели
;; (копия в assets/models/<name>/LICENSE.md).
;;
;; Честный бюджет кадров (замер печатается раз в 60 кадров): волны
;; обновляются каждый 2-й кадр — иначе Lisp-цикл волн съедал кадр.

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

;; высота острова: радиальный купол + синусный «шум», детерминирован;
;; за краем — дно уходит под воду
(define (isl-h x z)
  (let* ((r (sqrt (+ (* x x) (* z z))))
         (dome (* 3.4 (max 0.0 (- 1.0 (/ (* r r) 220.0)))))
         (n (+ (* 0.5 (sin (+ (* x 0.7) (* z 1.3))))
               (* 0.35 (cos (+ (* x 1.9) (* z 0.5))))))
         (drop (* 1.6 (max 0.0 (- r 13.0))))
         (h (+ dome n (- drop))))
    (if (< h -1.2) -1.2 h)))

;; волна: сумма трёх синусов (детерминирована, параметр — время)
(define (wave-y x z t)
  (+ (* 0.14 (sin (+ (* 1.1 x) (* 2.0 t))))
     (* 0.10 (sin (+ (* 1.4 z) (* -1.6 t))))
     (* 0.05 (sin (+ (* 2.3 (+ x z)) (* 3.1 t))))))

;; ——— сетка-хелпер: вершины/UV/индексы плоскости XZ ———
;; hfn — функция высоты (x z) → y; возвращает список (pos uv idx-f32vec)
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

;; ——— остров (высотное поле) и вода (обновляется волнами) ———
(define *island* (grid-make *ia* *in* isl-h))
(define *water-raw* (grid-make *wa* *wn* (lambda (x z) 0.0)))
(define *wpos* (car *water-raw*))      ; волнами обновляем только y
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

;; ——— процедурные текстуры (canvas на F32Vec, как в textures.lisp) ———
(define (isl-px! b w x y r g bb)   ; непрозрачный пиксель
  (let ((i (* 4 (+ (* y w) x))))
    (f32set! b i r) (f32set! b (+ i 1) g)
    (f32set! b (+ i 2) bb) (f32set! b (+ i 3) 1.0)))
(define (isl-pxa! b w x y r g bb a) ; пиксель с альфой (листва)
  (let ((i (* 4 (+ (* y w) x))))
    (f32set! b i r) (f32set! b (+ i 1) g)
    (f32set! b (+ i 2) bb) (f32set! b (+ i 3) a)))

;; трава: зелёный фон + тёмные «травинки» вертикальной модуляцией
(define (isl-tex-grass n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((k (/ y n))
               (base (+ 0.30 (* 0.22 (cos (+ (* x 0.9) (* y 0.4))))))
               (blade (* 0.14 (sin (+ (* x 5.0) (* y 1.7)))))
               (g (clamp (+ base blade (* 0.06 k)) 0.12 0.8)))
          (isl-px! b n x y (* 0.55 g) g (* 0.42 g)))))
    (texture-upload n n b)))

;; вода: бирюзово-синяя с бликовыми полосами (блик даёт материал)
(define (isl-tex-water n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let ((c (clamp (+ 0.55
                           (* 0.10 (sin (+ (* x 0.8) (* y 2.6)))
                                  (cos (+ (* x 3.1) (* y 0.9)))))
                        0.3 0.8)))
          (isl-px! b n x y (* 0.10 c) (* 0.45 c) (* 0.85 c)))))
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

;; листва: зелёные пятна с ДЫРКАМИ (alpha=0) — под alpha-test 0.5
(define (isl-tex-leaves n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((blob (+ (sin (+ (* x 0.9) (* y 1.3)))
                        (cos (+ (* x 1.7) (* y 0.4)))))
               (hole (and (> blob 0.9) (> (mod (+ x (* y 3)) 7) 4)))
               (g (clamp (+ 0.42 (* 0.3 blob)) 0.15 0.85)))
          (if hole
              (isl-pxa! b n x y 0 0 0 0.0)
              (isl-pxa! b n x y (* 0.45 g) g (* 0.3 g) 1.0)))))
    (texture-upload n n b)))

;; камень: серый с трещинами
(define (isl-tex-rock n)
  (let ((b (make-f32vec (* n n 4) 0.0)))
    (dotimes (y n)
      (dotimes (x n)
        (let* ((k (+ 0.52 (* 0.20 (sin (+ (* x 1.3) (* y 0.7))))
                     (* 0.10 (cos (+ (* x 3.7) (* y 2.9))))))
               (crack (< (abs (sin (+ (* x 0.5) (* y 3.3)))) 0.06))
               (v (if crack (* 0.5 k) k)))
          (isl-px! b n x y v v (* 1.06 v)))))
    (texture-upload n n b)))

;; ——— материалы (этап 7) ———
(defmaterial 'трава ':tiling '(7 7))
(defmaterial 'вода ':tiling '(5 5) ':spec 0.85 ':shininess 60)
(defmaterial 'кора ':tiling '(2 3))
(defmaterial 'листва ':alpha-test 0.5)
(defmaterial 'камень ':spec 0.12 ':shininess 24)
(defmaterial 'глинт ':spec 0.4 ':shininess 40)

;; ——— деревья: стволы (батч кубов) + кроны (2 скрещенных квадрата с
;; alpha-test), позиции детерминированы «золотым углом» + псевдорадиусом ———
(define *tree-n* 16)
(define (tree-pos k)
  (let* ((ang (* k 2.39996323))
         (rad (+ 3.2 (* 6.5 (/ (mod (+ (* k 37) 11) 19) 18.0))))
         (x (* rad (cos ang)))
         (z (* rad (sin ang))))
    (list x (isl-h x z) z)))
(define (tree-rows)
  (define (go k acc)
    (if (>= k *tree-n*)
        acc
        (let* ((p (tree-pos k))
               (s (+ 0.85 (* 0.5 (/ (mod (* k 13) 7) 6.0)))))
          (go (+ k 1)
              (append acc
                      (list (car p) (+ (cadr p) (* 0.9 s)) (caddr p)
                            (* 0.22 s) (* 1.9 s) (* 0.22 s)
                            (* k 1.7) 0 0 1 1 1))))))
  (go 0 '()))
(define *trunk-insts* (tree-rows))
(define *trunk-mesh* (mesh-cube 1.0))
(define *trunk-uv* (mesh-cube-uv))

;; крона: два скрещенных квадрата, обе стороны (16 треугольников)
(define *crown-pos*
  (f32-from-list (list -1 0 0   1 0 0   1 2 0   -1 2 0
                       0 0 -1   0 0 1   0 2 1   0 2 -1)))
(define *crown-uv*
  (f32-from-list (list 0 0 1 0 1 1 0 1   0 0 1 0 1 1 0 1)))
(define *crown-idx*
  (f32-from-list (list 0 1 2  0 2 3   2 1 0  3 2 0
                       4 5 6  4 6 7   6 5 4  7 6 4)))
(define (crown-insts)
  (define (go k acc)
    (if (>= k *tree-n*)
        acc
        (let* ((p (tree-pos k))
               (s (+ 1.5 (* 0.9 (/ (mod (* k 13) 7) 6.0)))))
          (go (+ k 1)
              (append acc
                      (list (car p) (+ (cadr p) (* 1.1 s)) (caddr p)
                            s s s (* k 1.7) 0 0 1 1 1))))))
  (go 0 '()))

;; камни: октаэдры (батч) с UV из позиции
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
    (if (>= k 8)
        acc
        (let* ((ang (+ 0.6 (* k 2.7)))
               (rad (+ 6.0 (* 4.0 (/ (mod (+ (* k 23) 5) 13) 12.0))))
               (x (* rad (cos ang)))
               (z (* rad (sin ang)))
               (s (+ 0.5 (* 0.45 (/ (mod (* k 7) 5) 4.0)))))
          (go (+ k 1)
              (append acc
                      (list x (+ (isl-h x z) (* 0.3 s)) z
                            (* 1.4 s) s (* 1.2 s)
                            (* k 2.1) 0.1 0 1 1 1))))))
  (go 0 '()))
(define *rock-insts* (rock-rows))

;; ——— glTF-модели у берега (пайплайн «модели из интернета») ———
;; CC0-1.0: KhronosGroup/glTF-Sample-Assets (лицензии в папках моделей)
(define *m-bottle*
  (if (file-exists? "assets/models/waterbottle/WaterBottle.gltf")
      (gltf-load "assets/models/waterbottle/WaterBottle.gltf")
      '()))
(define *m-lantern*
  (if (file-exists? "assets/models/lantern/Lantern.gltf")
      (gltf-load "assets/models/lantern/Lantern.gltf")
      '()))

(define (model-mat x y z s yaw)
  (mat4-mul (mat-translate x y z)
            (mat4-mul (mat-rotate-y yaw) (mat-scale s s s))))

;; ——— сцена ———
(define (island-tris)
  (+ (/ (f32vec-length (caddr *island*)) 3)
     (/ (f32vec-length *widx*) 3)
     (* *tree-n* 16)
     24))

(define (draw-sky)
  ;; тёплый градиент от горизонта (туман) к зениту — 2D-полосы до 3D
  (define (go i y)
    (if (>= i 12)
        #f
        (let ((k (/ i 11.0)))
          (draw-rect 0 y *screen-w* 20
                     (list (+ 0.52 (* 0.14 k))
                           (+ 0.66 (* 0.12 k))
                           (+ 0.82 (* 0.08 k))))
          (go (+ i 1) (+ y 20)))))
  (go 0 0))

(define (draw-scene)
  (let* ((t *t*)
         (a (* t 0.12))
         (eye (list (* 26.0 (cos a)) 9.5 (* 26.0 (sin a))))
         (mvp (camera-mvp eye (list 0 1.4 0) 0.95)))
    (gpu3d-view (car eye) (cadr eye) (caddr eye))
    ;; тёплое «солнце» + туман цвета неба
    (gpu3d-light 0.5 0.75 0.4 0.42 0.8)
    (gpu3d-fog 0.62 0.78 0.9 0.012)
    ;; остров — трава с тайлингом
    (draw-mesh-tex (list (car *island*) (caddr *island*)) (cadr *island*) *tex-grass*
                   (mat-identity) mvp (list 1 1 1) 'трава)
    ;; стволы — БАТЧ + кора; кроны — БАТЧ + alpha-test
    (draw-batch-tex *trunk-mesh* *trunk-uv* *tex-bark*
                    *trunk-insts* mvp 'кора)
    (draw-batch-tex (list *crown-pos* *crown-idx*) *crown-uv* *tex-leaves*
                    (crown-insts) mvp 'листва)
    ;; камни — батч с rock-текстурой
    (draw-batch-tex (list *rock-pos* *rock-idx*) *rock-uv* *tex-rock*
                    *rock-insts* mvp 'камень)
    ;; glTF-модели (draw-gltf из load-gltf.lisp)
    (if (null? *m-bottle*)
        #f
        (draw-gltf *m-bottle*
                   (model-mat 4.5 (+ (isl-h 4.5 2.0) 0.1) 2.0 9.0 0.7)
                   mvp (list 1 1 1)))
    (if (null? *m-lantern*)
        #f
        (draw-gltf *m-lantern*
                   (model-mat -3.0 (+ (isl-h -3.0 4.0) 0.1) 4.0 7.0 -0.5)
                   mvp (list 1 1 1) 'глинт))
    ;; вода — последней; блики через материал 'вода
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
  ;; честный fps: раз в 60 кадров — реальная скорость headless-рендера
  (if (>= *frame* (+ *fps-last* 60.0))
      (begin
        (set! *fps-last-val*
              (/ 60.0 (max 0.001 (- (time-now) *fps-clock*))))
        (set! *fps-clock* (time-now))
        (set! *fps-last* *frame*)
        (print (string-append "ISLAND fps: "
                              (number->string *fps-last-val*))))
      #f)
  #t)

;; ——— текстуры и часы (после всех определений) ———
(define *tex-grass* (isl-tex-grass 64))
(define *tex-water* (isl-tex-water 64))
(define *tex-bark* (isl-tex-bark 64))
(define *tex-leaves* (isl-tex-leaves 64))
(define *tex-rock* (isl-tex-rock 64))
(define *fps-clock* (time-now))
(print "ISLAND: остров/вода/деревья/камни готовы; glTF-мешей: "
       (+ (length *m-bottle*) (length *m-lantern*)))
