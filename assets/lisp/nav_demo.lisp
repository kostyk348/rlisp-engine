;; nav_demo.lisp — A* по навигационной сетке: враг ищет дорогу в лабиринте.
;;
;; Зачем сцена: навигация — первое, чего не хватало игровой логике. Раньше
;; каждый враг в РУИНЕРЕ «чуял» игрока по прямой и застревал в стенах; теперь
;; путь считает ядро (src/nav.rs), а сцена только спрашивает клетки.
;;
;; Что показывает:
;;   * сетка 40x30 строится из кирпичного узора (детерминированно, без random);
;;   * цель выбирается (random) с явным seed — значит прогон воспроизводим;
;;   * путь запрашивается нативом (nav-path) и агент идёт по клеткам;
;;   * печатается ASCII-карта с путём — доказательство работает и без GPU.
;;
;;   cargo run --release -- --script assets/lisp/nav_demo.lisp --fast --frames 600
;;   cargo run --release -- --script assets/lisp/nav_demo.lisp \
;;       --fast --frames 600 --record saves/nav.rlrp     # и затем --replay

(load "core.lisp")
(load "font.lisp")
(load "graphics.lisp")

(define *cw* 40)
(define *ch* 30)
(define *cs* 8)
(define *grid* 0)
(define *path* '())
(define *pi* 0)
(define *ax* 2)
(define *ay* 2)
(define *gx* 37)
(define *gy* 27)
(define *arrived* 0)
(define *spawned* 0)
(define *prints* 0)

(random-seed 20261015)

;; —— сетка: кирпичная кладка с проходами ————————————————————————————
(define (build-maze)
  (set! *grid* (nav-new *cw* *ch*))
  (dotimes (y *ch*)
    (if (and (> y 0) (< y (- *ch* 1)) (= (mod y 5) 0))
        (dotimes (x *cw*)
          ;; в каждом ряду кладки — три прохода (x mod 11 = 0,4,8)
          (if (> (mod x 11) 4)
              (if (> (mod x 11) 8)
                  (nav-block! *grid* x y #f)
                  (nav-block! *grid* x y #t))
              (nav-block! *grid* x y #f)))))
  ;; рамка
  (dotimes (x *cw*)
    (nav-block! *grid* x 0 #t)
    (nav-block! *grid* x (- *ch* 1) #t))
  (dotimes (y *ch*)
    (nav-block! *grid* 0 y #t)
    (nav-block! *grid* (- *cw* 1) y #t)))

;; —— цель и путь ————————————————————————————————————————————————
(define (pick-goal)
  (let ((x (+ 1 (random (- *cw* 2))))
        (y (+ 1 (random (- *ch* 2)))))
    (if (nav-free? *grid* x y) (list x y) (pick-goal))))

(define (retarget)
  (set! *spawned* (+ *spawned* 1))
  (let ((g (pick-goal)))
    (set! *gx* (car g))
    (set! *gy* (car (cdr g)))
    (set! *path* (nav-path *grid* *ax* *ay* *gx* *gy*))
    (set! *pi* 0)
    (if (and *path* (> *prints* 0))
        (print (string-append "путь " (number->string *ax*) ","
                              (number->string *ay*) " → "
                              (number->string *gx*) "," (number->string *gy*)
                              ": клеток " (number->string (/ (length *path*) 2)))))))

;; —— ASCII-карта: доказательство без GPU ——————————————————————————
(define (print-map)
  (set! *prints* (+ *prints* 1))
  (print "——— карта (w = стена, * = путь, A = агент, G = цель) ———")
  (dotimes (y *ch*)
    (let ((row ""))
      (dotimes (x *cw*)
        (let ((ch (if (not (nav-free? *grid* x y))
                      "w"
                      (if (and (= x *ax*) (= y *ay*))
                          "A"
                          (if (and (= x *gx*) (= y *gy*))
                              "G"
                              (if (on-path? x y) "*" "."))))))
          (set! row (string-append row ch))))
      (print row))))

(define (on-path? x y)
  (let ((found #f) (i 0))
    (while (< i (length *path*))
      (if (and (= (nth *path* i) x) (= (nth *path* (+ i 1)) y))
          (set! found #t))
      (set! i (+ i 2)))
    found))

;; —— кадр ————————————————————————————————————————————————————
(define (update dt)
  (if (or (not *path*) (>= *pi* (length *path*)))
      (retarget))
  ;; один шаг по клетке за кадр — сетка дискретная, так честнее
  (if (and *path* (< *pi* (length *path*)))
      (begin
        (set! *ax* (nth *path* *pi*))
        (set! *ay* (nth *path* (+ *pi* 1)))
        (set! *pi* (+ *pi* 2))
        (if (>= *pi* (length *path*))
            (set! *arrived* (+ *arrived* 1))))))

(define (draw)
  (gpu-clear 0.04 0.05 0.07)
  ;; свободные клетки — еле заметная сетка
  (dotimes (y *ch*)
    (dotimes (x *cw*)
      (if (not (nav-free? *grid* x y))
          (gpu-fill-rect (* x *cs*) (* y *cs*) *cs* *cs* 0.16 0.18 0.24))))
  ;; путь — голубые точки
  (let ((i 0))
    (while (< i (length *path*))
      (gpu-fill-rect (+ 2 (* (nth *path* i) *cs*)) (+ 2 (* (nth *path* (+ i 1)) *cs*))
                     4 4 0.15 0.85 1.0)
      (set! i (+ i 2))))
  ;; цель и агент
  (gpu-fill-rect (* *gx* *cs*) (* *gy* *cs*) *cs* *cs* 0.9 0.75 0.2)
  (gpu-fill-rect (* *ax* *cs*) (* *ay* *cs*) *cs* *cs* 1.0 0.3 0.35)
  (draw-text (string-append "PATHS " (number->string *spawned*)
                            "  ARRIVED " (number->string *arrived*))
             4 4 1 (list 1.0 1.0 1.0))
  (gpu-present))

(define (init)
  (build-maze)
  (retarget)
  (print (string-append "сетка " (number->string *cw*) "x" (number->string *ch*)
                        ", стен " (number->string (table-get (nav-stats *grid*) 'blocked 0))
                        ", старт " (number->string *ax*) "," (number->string *ay*)
                        ", цель " (number->string *gx*) "," (number->string *gy*)
                        ", клеток в пути " (number->string (/ (length *path*) 2))))
  (print-map))

;; сцена запускает себя сама: движок зовёт только update/draw
(init)
