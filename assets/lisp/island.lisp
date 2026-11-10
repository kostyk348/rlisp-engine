;; island.lisp — Уровень 4: сцена «ОСТРОВ» (этап 9, графическая ревизия 3).
;; Новое против ревизии 2:
;;   1. ТЕКСТУРЫ из PNG (scripts/gen_textures.py, процедурные и тайлящиеся):
;;      трава/песок/камень/кора 256², вместо 64² синусных заглушек;
;;   2. ALPHA-BLENDING (этап 9 движка): прозрачная очередь far→near,
;;      src-alpha, z-тест без z-записи, двусторонние грани;
;;   3. ВОДА с per-vertex цветом/альфой (gpu3d-mesh-vc): глубинный градиент
;;      бирюза→тёмная, ФРЕНЕЛЬ (отражение неба под углом), ПЕНА у берега,
;;      солнечный блик; сквозь воду видно дно;
;;   4. ПАЛЬМЫ на пляже: изогнутый ствол (5 сегментов) + 7 поникающих
;;      листьев-текстур, покачивание на ветру;
;;   5. Скалы на КРУТЫХ СКЛОНАХ (по градиенту высоты), не только на вершине;
;;   6. МЯГКИЕ ТЕНИ-блобы под деревьями (glow-текстура, блендинг);
;;   7. Пучки травы-билборды, ОБЛАКА-билборды, солнечное гало, небо
;;      из 12 полос с тёплым горизонтом.
;;
;; Текстуры детерминированы (seed в gen_textures.py), сцена повторяема.
;; glTF-модели — KhronosGroup/glTF-Sample-Assets, CC0-1.0 (см. credits.md).

(window-create *screen-w* *screen-h* "ISLAND :: rlisp-engine 3D")

;; ——— persistent-состояние: переживает hot reload ———
(defpersistent *t* 14.0) ;; старт орбиты между пальмами (без листьев в объективе)
(defpersistent *frame* 0)
(defpersistent *fps-last* 0.0)
(defpersistent *fps-last-val* 0.0)

;; ——— мир ———
(define *ia* 32.0)            ; полуразмер ландшафта (вместе с дном)
(define *in* 36)              ; клеток ландшафта (37×37 вершин)
(define *wa* 56.0)            ; полуразмер водного плана (угол сетки за туманом)
(define *wn* 20)              ; клеток воды (21×21 вершин)

(define (hash2 x y)
  (abs (sin (+ (* x 12.9898) (* y 78.233)))))

;; высота острова: купол + шум + БУХТА на востоке (понижение гауссом)
(define (isl-h x z)
  (let* ((r (sqrt (+ (* x x) (* z z))))
         (q (max 0.0 (- 1.0 (/ (* r r) 300.0))))
         (dome (* 3.6 q q))
         (n (+ (* 0.42 (sin (+ (* x 0.55) (* z 0.95))))
               (* 0.28 (cos (+ (* x 1.3) (* z -0.6))))
               (* 0.15 (sin (+ (* x 2.4) (* z 1.9))))
               (* 0.10 (sin (* x 4.1)))
               (* 0.10 (cos (* z 3.7)))))
         (cove (- 1.9 (* 1.9 (/ 1.0 (+ 1.0 (/ (+ (* (- x 15) (- x 15))
                                                 (* (- z 3) (- z 3)))
                                              30.0))))))
         (h0 (+ dome n (- (max 0.0 cove))))
         (h1 (max -2.2 (if (< h0 0.6) (+ 0.6 (* 1.7 (- h0 0.6))) h0))))
    (if (> r 21.0)
        (min h1 (- -0.35 (* 0.28 (- r 21.0))))
        h1)))

;; крутизна склона (для скал): модуль градиента высот
(define (isl-slope x z)
  (let ((e 0.7))
    (let ((gx (- (isl-h (+ x e) z) (isl-h (- x e) z)))
          (gz (- (isl-h x (+ z e)) (isl-h x (- z e)))))
      (sqrt (+ (* gx gx) (* gz gz))))))

;; волна: сумма трёх синусов (детерминирована, параметр — время)
(define (wave-y x z t)
  (+ (* 0.16 (sin (+ (* 1.1 x) (* 2.0 t))))
     (* 0.11 (sin (+ (* 1.4 z) (* -1.6 t))))
     (* 0.06 (sin (+ (* 2.3 (+ x z)) (* 3.1 t))))))

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

;; ——— ландшафт: один вершинный буфер, ЧЕТЫРЕ полосы ———
(define *island* (grid-make *ia* *in* isl-h))
(define *ipos* (car *island*))
(define *iuv* (cadr *island*))

(define (band-jitter x z)
  (+ (* 0.05 (sin (+ (* x 0.9) (* z 1.4))))
     (* 0.04 (sin (+ (* x 1.7) (* z -0.8))))))

;; полосы: 0 пляж-кольцо, 1 трава, 2 скалы (крутой склон ИЛИ вершина),
;; 3 морское дно (за островом — темнее через тинт при отрисовке)
(define (cell-band x z)
  (let* ((h (isl-h x z))
         (j (band-jitter x z))
         (r (sqrt (+ (* x x) (* z z))))
         (rb (+ 10.4 (* 1.3 (sin (+ (* x 0.8) (* z 0.6)))))))
    (cond ((> r 21.0) 3)
          ((> r rb) 0)
          ((and (> h 0.8) (> (isl-slope x z) (+ 1.75 (* 0.30 j)))) 2)
          ((> h (+ 2.85 j)) 2)
          (else 1))))

(define (grid-split-idx)
  (define (vi i j) (+ (* i (+ *in* 1)) j))
  (define (vx i) (- (* 2 *ia* (/ i *in*)) *ia*))
  (define (vz j) (- (* 2 *ia* (/ j *in*)) *ia*))
  (let ((sand '()) (grass '()) (rock '()) (sea '()))
    (define (go-cols i j)
      (if (>= j *in*)
          #f
          (let* ((b (cell-band (/ (+ (vx i) (vx (+ i 1))) 2.0)
                               (/ (+ (vz j) (vz (+ j 1))) 2.0)))
                 (q (list (vi i j) (vi i (+ j 1)) (vi (+ i 1) (+ j 1))
                          (vi i j) (vi (+ i 1) (+ j 1)) (vi (+ i 1) j))))
            (cond ((= b 0) (set! sand (append sand q)))
                  ((= b 1) (set! grass (append grass q)))
                  ((= b 2) (set! rock (append rock q)))
                  (else (set! sea (append sea q))))
            (go-cols i (+ j 1)))))
    (define (go-rows i)
      (if (>= i *in*)
          #f
          (begin (go-cols i 0) (go-rows (+ i 1)))))
    (go-rows 0)
    (list (f32-from-list sand)
          (f32-from-list grass)
          (f32-from-list rock)
          (f32-from-list sea))))

(define *bands* (grid-split-idx))
(define *idx-sand* (car *bands*))
(define *idx-grass* (cadr *bands*))
(define *idx-rock* (caddr *bands*))
(define *idx-sea* (cadddr *bands*))

;; ——— вода: per-vertex RGBA (глубина/френель/пена) ———
(define *water-raw* (grid-make *wa* *wn* (lambda (x z) 0.0)))
(define *wpos* (car *water-raw*))
(define *wuv* (cadr *water-raw*))
(define *widx* (caddr *water-raw*))
(define *wvc* (make-f32vec (* (+ *wn* 1) (+ *wn* 1) 4) 1.0))
;; статично: высота дна под вершиной воды
(define *wterr* (make-f32vec (* (+ *wn* 1) (+ *wn* 1)) 0.0))
(define (water-init!)
  (define (vx i) (- (* 2 *wa* (/ i *wn*)) *wa*))
  (define (vz j) (- (* 2 *wa* (/ j *wn*)) *wa*))
  (dotimes (i (+ *wn* 1))
    (dotimes (j (+ *wn* 1))
      (f32set! *wterr* (+ (* i (+ *wn* 1)) j) (isl-h (vx i) (vz j)))))
  #t)
(water-init!)

;; цвета воды (константы сцены)
(define *wv-deep*  (list 0.10 0.33 0.52))
(define *wv-shal*  (list 0.42 0.80 0.72))
(define *wv-sky*   (list 0.68 0.80 0.90))
(define *wv-foam*  (list 0.93 0.96 0.94))

(define (update-water-vc eye)
  (define (vx i) (- (* 2 *wa* (/ i *wn*)) *wa*))
  (define (vz j) (- (* 2 *wa* (/ j *wn*)) *wa*))
  (dotimes (i (+ *wn* 1))
    (dotimes (j (+ *wn* 1))
      (let* ((k4 (* 4 (+ (* i (+ *wn* 1)) j)))
             (kt (+ (* i (+ *wn* 1)) j))
             (x (vx i)) (z (vz j))
             (wy (f32ref *wpos* (+ (* kt 3) 1)))
             (th (f32ref *wterr* kt))
             (d (max 0.05 (- wy th)))                 ; толщина воды
             ;; глубинный цвет: бирюза у берега → тёмная на глубине
             (dk (min 1.0 (/ d 3.2)))
             (cr (+ (* (car *wv-shal*) (- 1.0 dk)) (* (car *wv-deep*) dk)))
             (cg (+ (* (cadr *wv-shal*) (- 1.0 dk)) (* (cadr *wv-deep*) dk)))
             (cb (+ (* (caddr *wv-shal*) (- 1.0 dk)) (* (caddr *wv-deep*) dk)))
             ;; френель: взгляд вдоль воды → отражение неба и альфа выше
             (vx_ (- (car eye) x)) (vy_ (- (cadr eye) wy)) (vz_ (- (caddr eye) z))
             (vl (max 0.001 (sqrt (+ (* vx_ vx_) (* vy_ vy_) (* vz_ vz_)))))
             (cosv (min 1.0 (abs (/ vy_ vl))))
             (fr (* (- 1.0 cosv) (- 1.0 cosv) (sqrt (- 1.0 cosv))))
             (frr (min 0.88 (* 1.15 (* fr (- 1.0 cosv)))))
             ;; пена: где под водой мелко И волна поднялась
             (fm (max 0.0 (min 1.0 (/ (- 0.42 d) 0.42))))
             (fm2 (* fm (max 0.0 (/ (+ (f32ref *wpos* (+ (* kt 3) 1)) 0.2) 0.5))))
             (fmix (min 1.0 (+ (* 0.5 fm) fm2)))
             (al (min 0.97 (+ 0.64 (* 0.3 frr) (* 0.3 fmix)))))
        (f32set! *wvc* k4
                 (+ (* cr (- 1.0 frr)) (* (car *wv-sky*) frr) (* fmix (- (car *wv-foam*) cr))))
        (f32set! *wvc* (+ k4 1)
                 (+ (* cg (- 1.0 frr)) (* (cadr *wv-sky*) frr) (* fmix (- (cadr *wv-foam*) cg))))
        (f32set! *wvc* (+ k4 2)
                 (+ (* cb (- 1.0 frr)) (* (caddr *wv-sky*) frr) (* fmix (- (caddr *wv-foam*) cb))))
        (f32set! *wvc* (+ k4 3) al))))
  #t)

;; геометрия воды: подъём вершин волнами (каждый 2-й кадр)
(define (update-water!)
  (define (vx i) (- (* 2 *wa* (/ i *wn*)) *wa*))
  (define (vz j) (- (* 2 *wa* (/ j *wn*)) *wa*))
  (dotimes (i (+ *wn* 1))
    (dotimes (j (+ *wn* 1))
      (f32set! *wpos* (+ (* (+ (* i (+ *wn* 1)) j) 3) 1)
               (wave-y (vx i) (vz j) *t*))))
  #t)

;; ——— PNG-текстуры (scripts/gen_textures.py) ———
(define (tex-load name)
  (texture-load-png (file-read-bytes (string-append "assets/textures/" name))))

(define *tex-grass* (tex-load "grass.png"))
(define *tex-sand*  (tex-load "sand.png"))
(define *tex-rock*  (tex-load "rock.png"))
(define *tex-bark*  (tex-load "bark.png"))
(define *tex-leaf*  (tex-load "leaf.png"))
(define *tex-palm*  (tex-load "palm.png"))
(define *tex-cloud* (tex-load "cloud.png"))
(define *tex-glow*  (tex-load "glow.png"))
(define *tex-tuft*  (tex-load "tuft.png"))

;; ——— материалы (этап 7) ———
(defmaterial 'песок  ':tiling '(5 5))
(defmaterial 'трава  ':tiling '(6 6))
(defmaterial 'камень ':tiling '(2 2) ':spec 0.18 ':shininess 26)
(defmaterial 'дно    ':tiling '(4 4))
(defmaterial 'кора   ':tiling '(2 3))
(defmaterial 'листва ':tiling '(1 1))
(defmaterial 'палма  ':tiling '(1 1))
(defmaterial 'вода   ':spec 0.85 ':shininess 26)
(defmaterial 'глинт  ':spec 0.4 ':shininess 40)

;; ——— широколиственные деревья: ствол + крона из скрещенных квадрантов
;; с МЯГКОЙ листвой (alpha-blend, без жёсткого cutout) ———
(define *tree-n* 12)

(define (tree-pos k)
  (let* ((ang (+ (* k 2.39996323) (* (mod k 3) 2.1)))
         (rad (+ 3.0 (* 4.5 (hash2 k 13))))
         (x (* rad (cos ang)))
         (z (* rad (sin ang))))
    (list x (isl-h x z) z)))

(define (tree-scale k) (+ 0.85 (* 0.55 (hash2 k 5))))

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
                            (* 0.14 s) (* 1.5 s) (* 0.14 s)
                            (* k 1.7) 0 0 0.82 0.70 0.56))))))
  (go 0 '()))

;; крона: 3 слоя × 2 скрещенных квадрата, ОБА бока (двусторонний blend)
(define (crown-geo)
  (let ((pos '()) (uv '()) (idx '()) (vi 0))
    (define (hw L) (nth (list 1.55 1.25 0.85) L))
    (define (y0 L) (nth (list 0.0 1.0 1.8) L))
    (define (y1 L) (+ (y0 L) (nth (list 1.7 1.4 1.05) L)))
    (define (quad ax ay az bx by bz cx cy cz dx dy dz)
      (set! pos (append pos (list ax ay az bx by bz cx cy cz dx dy dz)))
      (set! uv (append uv (list 0 0 1 0 1 1 0 1)))
      (set! idx (append idx (list vi (+ vi 1) (+ vi 2)  vi (+ vi 2) (+ vi 3)
                                  (+ vi 2) (+ vi 1) vi  (+ vi 3) (+ vi 2) vi)))
      (set! vi (+ vi 4)))
    (dotimes (L 3)
      (let ((w (hw L)) (a (y0 L)) (bb (y1 L)))
        (quad (- w) a 0  w a 0  w bb 0  (- w) bb 0)
        (quad 0 a (- w)  0 a w  0 bb w  0 bb (- w))))
    (list (f32-from-list pos) (f32-from-list uv) (f32-from-list idx))))

(define *crown* (crown-geo))

;; кроны с покачиванием: пересобираются каждый 2-й кадр
(define (crown-insts)
  (define (go k acc)
    (if (>= k *tree-n*)
        acc
        (let* ((p (tree-pos k))
               (s (tree-scale k))
               (sh (+ 0.85 (* 0.3 (hash2 k 23))))
               (sw (* 0.045 (sin (+ (* *t* 1.6) (* k 1.9))))))
          (go (+ k 1)
              (append acc
                      (list (car p) (+ (cadr p) (* 1.6 s)) (caddr p)
                            s s s (+ (* k 1.7) sw) sw 0
                            (* 0.92 sh) sh (* 0.78 sh)))))))
  (go 0 '()))

;; ——— пальмы на пляже: изогнутый ствол (5 сегментов) + 7 листьев ———
(define *palm-n* 7)

(define (palm-pos k)
  (let* ((ang (+ 1.15 (* k 0.897) (* 0.4 (hash2 k 61))))
         (rad (+ 11.6 (* 1.8 (hash2 k 67))))
         (x (* rad (cos ang)))
         (z (* rad (sin ang))))
    (list x (isl-h x z) z)))

;; ствол: сегменты с нарастающим наклоном (lean — азимут изгиба)
(define (palm-trunk-insts)
  (let ((acc '()))
    (dotimes (k *palm-n*)
      (let* ((p (palm-pos k))
             (s (+ 0.9 (* 0.3 (hash2 k 71))))
             (lean-az (* 6.28 (hash2 k 73)))
             (lx (cos lean-az))
             (lz (sin lean-az)))
        (dotimes (q 5)
          (let* ((tt (/ q 5.0))
                 (px (+ (car p) (* lx tt tt 1.5 s)))
                 (pz (+ (caddr p) (* lz tt tt 1.5 s)))
                 (py (+ (cadr p) (* 0.5 s) (* q 0.78 s))))
            (set! acc
                  (append acc
                          (list px py pz
                                (- 0.30 (* 0.035 q)) (* 0.80 s) (- 0.30 (* 0.035 q))
                                lean-az (- 0.22 tt) 0
                                0.80 0.66 0.50)))))))
    acc))

;; верхушка ствола (для листьев): азимут изгиба + высота
(define (palm-top k)
  (let* ((p (palm-pos k))
         (s (+ 0.9 (* 0.3 (hash2 k 71))))
         (lean-az (* 6.28 (hash2 k 73)))
         (lx (cos lean-az)) (lz (sin lean-az)))
    (list (+ (car p) (* lx 1.5 s))
          (+ (+ (cadr p) (* 0.5 s)) (* 5 0.78 s))
          (+ (caddr p) (* lz 1.5 s))
          s)))

;; листья: локальный квад вдоль +x, roll (Rz) топит его вниз, yaw раскладывает
;; веером; пересобираются каждый 2-й кадр (ветер)
(define (palm-frond-insts)
  (let ((acc '()))
    (dotimes (k *palm-n*)
      (let* ((top (palm-top k))
             (s (nth top 3)))
        (dotimes (f 7)
          (let* ((az (+ (* f 0.8976) (* 0.5 (hash2 k (+ 80 f)))))
                 (droop (- -0.55 (* 0.55 (/ (mod f 3) 2.0)) (* 0.1 (hash2 k f))))
                 (sw (* 0.05 (sin (+ (* *t* 2.1) (* k 2.3) (* f 0.7)))))
                 (L (* 3.05 s (+ 0.85 (* 0.3 (hash2 k (+ 90 f)))))))
            (set! acc
                  (append acc
                          (list (nth top 0) (- (nth top 1) (* 0.25 s)) (nth top 2)
                                L (* 1.7 s) 1.0
                                az 0 (- droop sw)
                                0.86 0.98 0.72)))))))
    acc))

;; геометрия листа: квад 0..1 по x, -0.5..0.5 по z (uv на всю текстуру)
;; (pos idx uv): квад 0..1 по x, -0.5..0.5 по z; uv — вся текстура
(define *frond-mesh*
  (list (f32-from-list (list 0 0 -0.5   1 0 -0.5   1 0 0.5   0 0 0.5))
        (f32-from-list (list 0 1 2  0 2 3))
        (f32-from-list (list 0 0 1 0 1 1 0 1))))

;; ——— мягкие тени-блобы (glow-текстура, чёрные, blend) ———
(define (shadow-quad pos size)
  (let ((x (car pos)) (y (cadr pos)) (z (caddr pos)) (h (/ size 2.0)))
    (list (- x h) y (- z h)   (+ x h) y (- z h)
          (+ x h) y (+ z h)   (- x h) y (+ z h))))
(define (shadow-append acc q)
  (append acc q))

(define (build-shadows)
  ;; один mesh-vc: все тени (21 квад = 84 вершины), y чуть над землёй
  (let* ((n 84)
         (pos (make-f32vec (* n 3) 0.0))
         (vc (make-f32vec (* n 4) 0.0))
         (idx '())
         (vi 0))
    (define (add pos-list)
      (dotimes (v 4)
        (let ((bx (* vi 3)) (bc (* vi 4)))
          (f32set! pos bx (nth pos-list (* v 3)))
          (f32set! pos (+ bx 1) (+ (nth pos-list (+ (* v 3) 1)) 0.20))
          (f32set! pos (+ bx 2) (nth pos-list (+ (* v 3) 2)))
          (f32set! vc bc 0.0)
          (f32set! vc (+ bc 1) 0.0)
          (f32set! vc (+ bc 2) 0.0)
          (f32set! vc (+ bc 3) 0.45))
        (set! vi (+ vi 1)))
      (set! idx (append idx (list (- vi 4) (- vi 3) (- vi 2)
                                  (- vi 4) (- vi 2) (- vi 1)))))
    (dotimes (k *tree-n*)
      (add (shadow-quad (tree-pos k) (* 2.6 (tree-scale k)))))
    (dotimes (k *palm-n*)
      (add (shadow-quad (palm-pos k) 2.0)))
    (add (shadow-quad (list 5.2 (isl-h 5.2 3.0) 3.0) 1.0))
    (add (shadow-quad (list -3.4 (isl-h -3.4 3.4) 3.4) 1.3))
    (list pos vc (f32-from-list idx))))

(define *shadows* (build-shadows))

;; uv-развёртка квада для каждой тени (glow-текстура даёт мягкий край)
(define (shadow-uv n-quads)
  (let ((uv (make-f32vec (* n-quads 4 2) 0.0)))
    (dotimes (q n-quads)
      (let ((b (* q 8)))
        (f32set! uv b 0)
        (f32set! uv (+ b 1) 0)
        (f32set! uv (+ b 2) 1)
        (f32set! uv (+ b 3) 0)
        (f32set! uv (+ b 4) 1)
        (f32set! uv (+ b 5) 1)
        (f32set! uv (+ b 6) 0)
        (f32set! uv (+ b 7) 1)))
    uv))

;; ——— камни: октаэдры (батч) ———
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
    (if (>= k 12)
        acc
        (let* ((ang (+ 0.4 (* k 2.7)))
               (rad (+ 4.5 (* 6.5 (hash2 k 31))))
               (x (* rad (cos ang)))
               (z (* rad (sin ang)))
               (s (+ 0.35 (* 0.5 (hash2 k 7)))))
          (go (+ k 1)
              (append acc
                      (list x (- (isl-h x z) (* 0.3 s)) z
                            (* 1.25 s) (* 0.6 s) (* 1.1 s)
                            (* k 2.1) 0.1 0 1 1 1))))))
  (go 0 '()))

;; ——— пучки травы: билборды-кресты на газоне ———
(define (tuft-mesh)
  ;; крест из двух квадов, основание в y=0
  (let ((pos '()) (idx '()) (vi 0))
    (define (quad ax ay az bx by bz cx cy cz dx dy dz)
      (set! pos (append pos (list ax ay az bx by bz cx cy cz dx dy dz)))
      (set! idx (append idx (list vi (+ vi 1) (+ vi 2)  vi (+ vi 2) (+ vi 3))))
      (set! vi (+ vi 4)))
    (quad -0.5 0 0  0.5 0 0  0.5 1 0  -0.5 1 0)
    (quad 0 0 -0.5  0 0 0.5  0 1 0.5  0 1 -0.5)
    (list (f32-from-list pos) (f32-from-list idx))))
(define *tuft-mesh* (tuft-mesh))
(define *tuft-uv* (f32-from-list (list 0 0 1 0 1 1 0 1  0 0 1 0 1 1 0 1)))

(define (tuft-insts)
  (define (go k acc)
    (if (>= k 56)
        acc
        (let* ((ang (* k 2.39996323))
               (rad (+ 2.5 (* 8.0 (hash2 k 101))))
               (x (* rad (cos ang)))
               (z (* rad (sin ang)))
               (h (isl-h x z))
               (g (+ 0.75 (* 0.4 (hash2 k 103)))))
          (if (or (< h 0.5) (> h 2.6))
              (go (+ k 1) acc)
              (go (+ k 1)
                  (append acc
                          (list x (- h 0.05) z
                                (+ 0.55 (* 0.5 (hash2 k 105)))
                                (+ 0.55 (* 0.5 (hash2 k 105))) 1.0
                                (* k 2.1) 0 0 (* 0.85 g) g (* 0.6 g))))))))
  (go 0 '()))

;; пучки не качаются — считаем ОДИН раз при загрузке
(define *tuft-cache* (tuft-insts))

;; ——— облака: медленные билборды высоко вокруг ———
(define (cloud-mesh)
  ;; горизонтальный квад в XZ — виден снизу с любого азимута орбиты
  (list (f32-from-list (list -0.5 0 -0.5  0.5 0 -0.5  0.5 0 0.5  -0.5 0 0.5))
        (f32-from-list (list 0 2 1  0 3 2))))
(define *cloud-mesh* (cloud-mesh))
(define *cloud-uv* (f32-from-list (list 0 0 1 0 1 1 0 1)))

(define (cloud-insts)
  (define (go k acc)
    (if (>= k 9)
        acc
        (let* ((ang (+ (* k 0.7) (* *t* 0.012)))
               (rad (+ 55 (* 30 (hash2 k 201))))
               (x (* rad (cos ang)))
               (z (* rad (sin ang)))
               (y (+ 26 (* 14 (hash2 k 203))))
               (s (+ 20 (* 14 (hash2 k 205)))))
          (go (+ k 1)
              (append acc
                      (list x y z s s 1.0 (* k 1.3) 0 0
                            1 1 1))))))
  (go 0 '()))

;; кэши инстансов (ветер/облака пересчитываются раз в N кадров, не каждый)
(defpersistent *crown-cache* '())
(defpersistent *frond-cache* '())
(defpersistent *cloud-cache* '())

;; ——— glTF-модели: нормализация по bbox (см. ревизию 2) ———
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

;; ——— счётчик треугольников (HUD) ———
(define (island-tris)
  (+ (/ (f32vec-length *idx-sand*) 3)
     (/ (f32vec-length *idx-grass*) 3)
     (/ (f32vec-length *idx-rock*) 3)
     (/ (f32vec-length *idx-sea*) 3)
     (/ (f32vec-length *widx*) 3)
     (* (+ *tree-n* *palm-n*) 24)
     100))

;; ——— небо: 12 полос, тёплый горизонт → глубокий зенит ———
(define (draw-sky)
  (define (go i y)
    (if (>= i 12)
        #f
        (let ((k (/ i 11.0)))
          (draw-rect 0 y *screen-w* 20
                     (list (- 0.76 (* 0.42 k))
                           (- 0.84 (* 0.30 k))
                           (- 0.91 (* 0.09 k))))
          (go (+ i 1) (+ y 20)))))
  (go 0 0))

;; солнечное гало: проект точки (eye + dir·80) на экран через mvp
(define (mvp-project mvp x y z)
  (let* ((cx (+ (* (f32ref mvp 0) x) (* (f32ref mvp 1) y)
                (* (f32ref mvp 2) z) (f32ref mvp 3)))
         (cy (+ (* (f32ref mvp 4) x) (* (f32ref mvp 5) y)
                (* (f32ref mvp 6) z) (f32ref mvp 7)))
         (cw (+ (* (f32ref mvp 12) x) (* (f32ref mvp 13) y)
                (* (f32ref mvp 14) z) (f32ref mvp 15))))
    (if (> cw 0.5)
        (let* ((wh (window-size))
               (sx (* (+ (/ cx cw) 0.5) (car wh)))
               (sy (* (- 1.0 (+ (/ cy cw) 0.5)) (cadr wh))))
          (list sx sy))
        #f)))

(define (draw-sun mvp)
  (let* ((eye (list (* 20.5 (cos (* *t* 0.10))) 8.2 (* 20.5 (sin (* *t* 0.10)))))
         (p (mvp-project mvp 60.0 95.0 45.0)))
    (if p
        (let ((sx (car p)) (sy (cadr p)))
          (draw-sprite *tex-glow* (- sx 38) (- sy 38) 76 (list 1.0 0.88 0.62)))
        #f)))

;; ——— сцена ———
(define (draw-scene)
  (let* ((eye (list (* 20.5 (cos (* *t* 0.10)))
                    (+ 9.6 (* 0.9 (sin (* *t* 0.071))))
                    (* 20.5 (sin (* *t* 0.10)))))
         (mvp (camera-mvp eye (list 0 2.2 0) 0.95)))
    ;; цвет = низ неба; ЧИСТИТ И Z-БУФЕР И ОЧЕРЕДЬ
    (gpu3d-clear 0.74 0.83 0.90)
    (gpu3d-view (car eye) (cadr eye) (caddr eye))
    (gpu3d-light 0.40 0.68 0.30 0.43 0.83 0.55 44)
    (gpu3d-fog 0.72 0.82 0.90 0.016)
    ;; ландшафт: 4 полосы одним вершинным буфером (дно темнее тинтом)
    (draw-mesh-tex (list *ipos* *idx-sand*) *iuv* *tex-sand*
                   (mat-identity) mvp (list 0.97 0.87 0.70) 'песок)
    (draw-mesh-tex (list *ipos* *idx-grass*) *iuv* *tex-grass*
                   (mat-identity) mvp (list 1 1 1) 'трава)
    (draw-mesh-tex (list *ipos* *idx-rock*) *iuv* *tex-rock*
                   (mat-identity) mvp (list 0.96 0.92 0.87) 'камень)
    (draw-mesh-tex (list *ipos* *idx-sea*) *iuv* *tex-sand*
                   (mat-identity) mvp (list 0.52 0.60 0.58) 'дно)
    ;; мягкие тени (blend, glow-текстура; tiling СБРОШЕН — иначе тинт
    ;; песка (5 5) режет радиальную текстуру на октагоны)
    (gpu3d-mat-tiling 1 1)
    (gpu3d-alpha-blend 1.0)
    (gpu3d-mesh-vc (nth *shadows* 0) (shadow-uv 21) (nth *shadows* 1)
                   (nth *shadows* 2) (mat-identity) mvp *tex-glow*)
    ;; трава-билборды (blend)
    (use-material! 'листва)
    (gpu3d-alpha-blend 0.95)
    (draw-batch-tex *tuft-mesh* *tuft-uv* *tex-tuft* *tuft-cache* mvp)
    ;; стволы деревьев (opaque) и кроны (blend)
    (gpu3d-alpha-blend 1.0)
    (draw-batch-tex *trunk-mesh* *trunk-uv* *tex-bark*
                    *trunk-cache* mvp 'кора)
    (gpu3d-alpha-blend 0.96)
    (draw-batch-tex (list (car *crown*) (caddr *crown*)) (cadr *crown*)
                    *tex-leaf* *crown-cache* mvp 'листва)
    ;; пальмы: ствол (opaque) + листья (blend, ветер)
    (gpu3d-alpha-blend 1.0)
    (draw-batch-tex *trunk-mesh* *trunk-uv* *tex-bark*
                    *ptrunk-cache* mvp 'кора)
    (gpu3d-alpha-blend 0.98)
    (draw-batch-tex (list (car *frond-mesh*) (cadr *frond-mesh*))
                    (caddr *frond-mesh*) *tex-palm* *frond-cache* mvp 'палма)
    ;; камни
    (gpu3d-alpha-blend 1.0)
    (draw-batch-tex (list *rock-pos* *rock-idx*) *rock-uv* *tex-rock*
                    *rock-cache* mvp 'камень)
    ;; glTF-модели
    (if (null? *m-bottle-draw*)
        #f
        (draw-gltf *m-bottle-draw*
                   (model-mat 5.2 (+ (isl-h 5.2 3.0) 0.05) 3.0 *s-bottle* 0.7)
                   mvp (list 1 1 1)))
    (if (null? *m-lantern-draw*)
        #f
        (draw-gltf *m-lantern-draw*
                   (model-mat -3.4 (+ (isl-h -3.4 3.4) 0.05) 3.4 (* 0.55 *s-lantern*) -0.5)
                   mvp (list 1 1 1) 'глинт))
    ;; облака (blend, далеко) — в общую прозрачную очередь с водой
    (gpu3d-alpha-blend 0.88)
    (draw-batch-tex *cloud-mesh* *cloud-uv* *tex-cloud* *cloud-cache* mvp)
    ;; вода: per-vertex цвет/альфа + блик материала 'вода
    (use-material! 'вода)
    (gpu3d-mesh-vc *wpos* *wuv* *wvc* *widx* (mat-identity) mvp 0)
    (material-reset!)
    ;; прозрачная очередь: сорт far→near + блендинг (вода/листва/облака/тени)
    (gpu3d-flush-alpha)
    mvp))

(define (draw-hud)
  (draw-text "ISLAND GFX2" 6 5 1 (list 1.0 1.0 1.0))
  (draw-text (string-append "TRIS " (number->string (island-tris)))
             6 13 1 (list 0.6 0.7 0.85))
  (draw-text (string-append "FPS " (number->string *fps-last-val*))
             6 21 1 (list 0.7 0.9 0.6)))

(define (refresh-caches!)
  (if (= 0 (mod *frame* 2))
      (begin
        (set! *crown-cache* (crown-insts))
        (set! *frond-cache* (palm-frond-insts)))
      #f)
  (if (= 0 (mod *frame* 6))
      (set! *cloud-cache* (cloud-insts))
      #f)
  #t)

(define (draw)
  (draw-sky)
  (let ((mvp (draw-scene)))
    (draw-sun mvp))
  (draw-hud)
  (gpu-present))

(define (update dt)
  (set! *t* (+ *t* dt))
  (set! *frame* (+ *frame* 1))
  (refresh-caches!)
  ;; геометрия волн — каждый 4-й кадр
  (if (= 0 (mod *frame* 4))
      (update-water!)
      #f)
  ;; цвет/альфа воды (френель/пена) — каждый 8-й кадр (френель меняется
  ;; медленно: камера орбитит 0.1 рад/с; честно отражено в LIMITATIONS)
  (if (= 0 (mod *frame* 8))
      (update-water-vc (list (* 20.5 (cos (* *t* 0.10)))
                             9.6
                             (* 20.5 (sin (* *t* 0.10)))))
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

;; статичные и анимированные кэши инстансов (после ВСЕХ определений)
(define *trunk-cache* (trunk-insts))
(define *ptrunk-cache* (palm-trunk-insts))
(define *rock-cache* (rock-rows))
(set! *crown-cache* (crown-insts))
(set! *frond-cache* (palm-frond-insts))
(set! *cloud-cache* (cloud-insts))
(define *fps-clock* (time-now))
(print "ISLAND GFX2: PNG-текстуры/alpha-blend/вода-fresnel+пена/пальмы/тени/облака готовы")
