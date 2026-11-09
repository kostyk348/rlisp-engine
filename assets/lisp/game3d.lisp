;; game3d.lisp — Уровень 4: 3D-демо «ORBITAL» (v0.2).
;; ВЕСЬ код ниже — Lisp: 3D-сцена, враги, пули, волны, камера, HUD.
;; Те же правила, что в 2D-«Выживании»: WASD, автоприцел, волны,
;; GAME OVER + рестарт на R, hot reload правит врагов на лету.
;; Отличие: мир объёмный — рендер через render3d.lisp поверх «GPU» HAL.
;;
;; v0.3: текстуры (пол — процедурная «неоновая схема», враги/игрок —
;; «панели» с тинтом), БАТЧ-ВЕРШИННИК (враги/пули/пилоны одним draw call),
;; хуки редактора/MCP (scene-entities, game-spawn, game-entity, game-remove!).

(window-create *screen-w* *screen-h* "ORBITAL :: rlisp-engine 3D")

;; ——— persistent-состояние: переживает hot reload (ТЗ §8) ———
(defpersistent *score* 0)
(defpersistent *kills* 0)
(defpersistent *wave* 1)
(defpersistent *player* (make-table))
(defpersistent *enemies* (make-table))
(defpersistent *pylons* (make-table))   ; этап 6: пропсы, спавнятся редактором
(defpersistent *bullets* '())
(defpersistent *fx* '())
(defpersistent *game-over* #f)
(defpersistent *spawn-timer* 1.2)
(defpersistent *wave-timer* 0.0)
(defpersistent *fire-timer* 0.0)
(defpersistent *next-id* 0)

;; игрок появляется один раз; при перезагрузке состояние восстанавливается
(if (table-has? *player* 'x) #f
    (begin (table-set! *player* 'x 0.0)
           (table-set! *player* 'z 0.0)))

;; ——— параметры мира (правьте на лету — работает без перезапуска) ———
(define *arena* 55)                 ; полуразмер арены
(define *player-speed* 26)
(define *fire-cooldown* 0.3)
(define *bullet-speed* 55)
(define *bullet-life* 1.5)

(define *grunt-speed* 16)                          ; ← sed в demo3d.sh правит на 30
(define *grunt-color* (list 0.95 0.25 0.25))       ; ← и цвет на оранжевый
(define *brute-speed* 9)
(define *brute-color* (list 0.7 0.4 0.95))
(define *floor-color* (list 0.11 0.10 0.20))
(define *grid-color* (list 0.10 0.34 0.40))
(define *torus-color* (list 0.95 0.7 0.2))
(define *pillar-color* (list 0.17 0.16 0.26))
(define *player-color* (list 0.15 0.85 0.95))
(define *bullet-color* (list 1.0 0.85 0.3))
(define *fx-color* (list 1.0 0.75 0.25))

;; ——— геометрия сцены: строится один раз при загрузке файла ———
(define *mesh-cube* (mesh-cube 1.0))
(define *mesh-octa* (mesh-octahedron 1.0))
(define *mesh-torus* (mesh-torus 6.5 1.8 20 9))
(define *mesh-floor* (mesh-plane 70 14))
(define *mesh-pillar* (mesh-cube 1.0))
(define *grid-wire* (list (mesh-pos *mesh-floor*) (mesh-plane-lines 14)))
(define *fx-wire* (mesh-to-wire *mesh-octa*))

;; ——— текстуры (v0.3): процедурные, создаются один раз при загрузке файла ———
(define *tex-floor* (tex-circuit 128 16))
(define *tex-panel* (tex-panel 64))
(define *uv-cube* (mesh-cube-uv))
(define *uv-floor* (mesh-plane-uv 14))

(define (px x) (table-get *player* 'x 0.0))         ; заглушка для читаемости
(define (time3d) (time-now))

;; ——— утилиты ———
(define (dist2d ax az bx bz)
  (sqrt (+ (* (- ax bx) (- ax bx)) (* (- az bz) (- az bz)))))

(define (dir-xz ax az bx bz)
  (let* ((dx (- bx ax)) (dz (- bz az))
         (l (max 1e-6 (sqrt (+ (* dx dx) (* dz dz))))))
    (list (/ dx l) (/ dz l))))

;; ——— враги ———
(define (spawn-enemy kind)
  (let* ((angle (* 6.28318530 (/ (random 1000) 1000.0)))
         (x (* 32 (cos angle)))
         (z (* 32 (sin angle)))
         (id *next-id*))
    (set! *next-id* (+ id 1))
    (let ((e (make-table)))
      (table-set! e 'x x)
      (table-set! e 'z z)
      (table-set! e 'hp (if (eq? kind 'brute) 3 1))
      (table-set! e 'kind kind)
      (table-set! e 'mul (+ 1.0 (* 0.06 (- *wave* 1))))
      (table-set! *enemies* id e))
    id))

(define (enemy-speed kind)
  (if (eq? kind 'brute) *brute-speed* *grunt-speed*))

(define (enemy-color kind)
  (if (eq? kind 'brute) *brute-color* *grunt-color*))

;; ——— пилоны (этап 6): статичные пропсы для drag-n-drop редактора ———
(define (spawn-pylon x z)
  (let ((id *next-id*))
    (set! *next-id* (+ id 1))
    (let ((p (make-table)))
      (table-set! p 'x x)
      (table-set! p 'z z)
      (table-set! p 'kind 'pylon)
      (table-set! *pylons* id p))
    id))

;; ——— стрельба: автоприцел по ближайшему врагу ———
(define (nearest-enemy-3d)
  (let ((best #f) (best-d 1e18))
    (for-each
     (lambda (entry)
       (let* ((e (cadr entry))
              (d (dist2d (px 0) (table-get *player* 'z 0.0)
                         (table-get e 'x 0.0) (table-get e 'z 0.0))))
         (if (< d best-d) (begin (set! best-d d) (set! best e)) #f)))
     (table-entries *enemies*))
    best))

(define (fire!)
  (if (> (length *bullets*) 40) #f
      (let ((target (nearest-enemy-3d)))
        (if target
            (let* ((bx (px 0)) (bz (table-get *player* 'z 0.0))
                   (d (dir-xz bx bz (table-get target 'x 0.0) (table-get target 'z 0.0)))
                   (b (make-table)))
              (table-set! b 'x (+ bx (* (car d) 2.0)))
              (table-set! b 'z (+ bz (* (cadr d) 2.0)))
              (table-set! b 'vx (* (car d) *bullet-speed*))
              (table-set! b 'vz (* (cadr d) *bullet-speed*))
              (table-set! b 'life *bullet-life*)
              (set! *bullets* (cons b *bullets*)))
            #f))))

(define (update-fire dt)
  (set! *fire-timer* (- *fire-timer* dt))
  (if (<= *fire-timer* 0.0)
      (begin (set! *fire-timer* *fire-cooldown*) (fire!))
      #f))

;; ——— пули: движение + попадания (расстояния в XZ) ———
(define (update-bullets dt)
  (for-each
   (lambda (b)
     (table-set! b 'x (+ (table-get b 'x 0.0) (* (table-get b 'vx 0.0) dt)))
     (table-set! b 'z (+ (table-get b 'z 0.0) (* (table-get b 'vz 0.0) dt)))
     (table-set! b 'life (- (table-get b 'life 0.0) dt)))
   *bullets*)
  (let ((entries (table-entries *enemies*)))
    (for-each
     (lambda (b)
       (for-each
        (lambda (entry)
          (let ((id (car entry)) (e (cadr entry)))
            (if (and (not (table-get b 'dead #f))
                     (< (dist2d (table-get b 'x 0.0) (table-get b 'z 0.0)
                                (table-get e 'x 0.0) (table-get e 'z 0.0)) 2.2))
                (let ((hp (- (table-get e 'hp 1) 1)))
                  (table-set! b 'dead #t)
                  (if (<= hp 0)
                      (begin (table-remove! *enemies* id)
                             (set! *score* (+ *score*
                                              (if (eq? (table-get e 'kind 'grunt) 'grunt) 10 30)))
                             (set! *kills* (+ *kills* 1))
                             (let ((fx (make-table)))
                               (table-set! fx 'x (table-get e 'x 0.0))
                               (table-set! fx 'z (table-get e 'z 0.0))
                               (table-set! fx 't 0.0)
                               (set! *fx* (cons fx *fx*))))
                      (table-set! e 'hp hp)))
                #f)))
        entries))
     *bullets*))
  (set! *bullets*
        (filter (lambda (b) (and (> (table-get b 'life 0.0) 0.0)
                                 (not (table-get b 'dead #f))
                                 (< (abs (table-get b 'x 0.0)) 90)
                                 (< (abs (table-get b 'z 0.0)) 90)))
                *bullets*))
  #t)

;; ——— эффекты взрывов ———
(define (update-fx dt)
  (for-each (lambda (fx) (table-set! fx 't (+ (table-get fx 't 0.0) dt))) *fx*)
  (set! *fx* (filter (lambda (fx) (< (table-get fx 't 0.0) 0.35)) *fx*))
  #t)

;; ——— игрок: WASD в плоскости XZ (w — «от камеры») ———
(define (update-player dt)
  (let ((dx 0) (dz 0))
    (if (key-down? "a") (set! dx (- dx 1)) #f)
    (if (key-down? "d") (set! dx (+ dx 1)) #f)
    (if (key-down? "w") (set! dz (- dz 1)) #f)
    (if (key-down? "s") (set! dz (+ dz 1)) #f)
    (let* ((diag (and (not (= dx 0)) (not (= dz 0))))
           (k (if diag 0.7071 1.0))
           (nx (clamp (+ (px 0) (* dx *player-speed* dt k)) (- *arena*) *arena*))
           (nz (clamp (+ (table-get *player* 'z 0.0) (* dz *player-speed* dt k))
                      (- *arena*) *arena*)))
      (table-set! *player* 'x nx)
      (table-set! *player* 'z nz))))

;; ——— преследование + волны ———
(define (update-enemies dt)
  (for-each
   (lambda (entry)
     (let* ((e (cadr entry))
            (d (dir-xz (table-get e 'x 0.0) (table-get e 'z 0.0)
                       (px 0) (table-get *player* 'z 0.0)))
            (sp (* (enemy-speed (table-get e 'kind 'grunt)) (table-get e 'mul 1.0))))
       (table-set! e 'x (+ (table-get e 'x 0.0) (* (car d) sp dt)))
       (table-set! e 'z (+ (table-get e 'z 0.0) (* (cadr d) sp dt)))))
   (table-entries *enemies*))
  #t)

(define (update-waves dt)
  (set! *spawn-timer* (- *spawn-timer* dt))
  (if (<= *spawn-timer* 0.0)
      (begin
        (spawn-enemy (if (and (> *wave* 1) (= (random 3) 0)) 'brute 'grunt))
        (set! *spawn-timer* (max 0.5 (- 2.4 (* 0.18 *wave*)))))
      #f)
  (set! *wave-timer* (+ *wave-timer* dt))
  (if (> *wave-timer* 12.0)
      (begin (set! *wave-timer* 0.0)
             (set! *wave* (+ *wave* 1))
             (spawn-enemy 'brute))
      #f))

(define (handle-collisions)
  (if *game-over* #f
      (begin
        (for-each
         (lambda (entry)
           (let ((e (cadr entry)))
             (if (and (not *game-over*)
                      (< (dist2d (px 0) (table-get *player* 'z 0.0)
                                 (table-get e 'x 0.0) (table-get e 'z 0.0)) 2.4))
                 (set! *game-over* #t)
                 #f)))
         (table-entries *enemies*))
        #t)))

(define (reset-game)
  (set! *score* 0) (set! *kills* 0) (set! *wave* 1)
  (set! *game-over* #f)
  (set! *bullets* '()) (set! *fx* '())
  (table-clear! *enemies*)
  (table-set! *player* 'x 0.0)
  (table-set! *player* 'z 0.0)
  (set! *spawn-timer* 1.2) (set! *wave-timer* 0.0) (set! *fire-timer* 0.0)
  (print "reset-game: новый забег в ORBITAL"))

(define (check-restart)
  (if (and *game-over* (key-pressed? "r")) (reset-game) #f))

;; ——— update: его вызывает Rust-ядро каждый кадр ———
(define (update dt)
  (poll-input)
  (if *game-over*
      (check-restart)
      (begin
        (update-player dt)
        (update-enemies dt)
        (update-fire dt)
        (update-bullets dt)
        (update-fx dt)
        (update-waves dt)
        (handle-collisions)
        #t)))

;; ——— отрисовка: 3D-сцена + HUD ———
(defmacro with-frame3d body
  `(begin (gpu3d-clear 0.02 0.02 0.055) ,@body (gpu-present)))

;; вертикальный градиент неба полосами (до 3D-объектов, поверх фона)
(define (draw-sky)
  (define (go i y)
    (if (>= i 20) #f
        (let ((k (/ i 19.0)))
          (draw-rect 0 y *screen-w* 13
                     (list (+ 0.01 (* 0.09 k))
                           (+ 0.01 (* 0.05 k))
                           (+ 0.04 (* 0.13 k))))
          (go (+ i 1) (+ y 12)))))
  (go 0 0))

;; ——— батч-сборка инстансов (v0.3): плоские списки по 12 f32 ———
(define (enemy-insts t)
  (define (go entries acc)
    (if (null? entries) acc
        (let* ((entry (car entries))
               (id (string->number (car entry)))
               (e (cadr entry))
               (c (enemy-color (table-get e 'kind 'grunt))))
          (go (cdr entries)
              (append acc (inst-row (table-get e 'x 0.0) 1.3 (table-get e 'z 0.0)
                                    2.0 (+ (* t 2.0) (* id 1.3))
                                    (car c) (cadr c) (caddr c)))))))
  (go (table-entries *enemies*) '()))

(define (bullet-insts t)
  (define (go lst acc)
    (if (null? lst) acc
        (let ((b (car lst)))
          (go (cdr lst)
              (append acc (inst-row-rpy (table-get b 'x 0.0) 1.4 (table-get b 'z 0.0)
                                        0.55 0.0 (* t 6.0) 0.0
                                        (car *bullet-color*) (cadr *bullet-color*)
                                        (caddr *bullet-color*)))))))
  (go *bullets* '()))

(define (pillar-row p)
  (list (car p) 13 (cadr p) 3.0 26.0 3.0 0.0 0.0 0.0
        (car *pillar-color*) (cadr *pillar-color*) (caddr *pillar-color*)))
(define *pillar-insts*
  (append (pillar-row (list -48 -48)) (pillar-row (list 48 -48))
          (pillar-row (list -48 48)) (pillar-row (list 48 48))))

;; редакторские пилоны (этап 6): ряд инстанса — 12 f32, куб вытянут вверх
(define (pylon-insts)
  (define (go entries acc)
    (if (null? entries) acc
        (let* ((entry (car entries))
               (p (cadr entry)))
          (go (cdr entries)
              (append acc (list (table-get p 'x 0.0) 5.0 (table-get p 'z 0.0)
                                2.2 10.0 2.2 0.0 0.0 0.0
                                (car *pillar-color*) (cadr *pillar-color*)
                                (caddr *pillar-color*)))))))
  (go (table-entries *pylons*) '()))

(define (draw-scene t)
  (let* ((pxv (px 0))
         (pzv (table-get *player* 'z 0.0))
         (mvp (camera-mvp (list (+ pxv (* 4 (sin (* t 0.25)))) 22 (+ pzv 27))
                          (list pxv 0 (- pzv 5))
                          1.0)))
    ;; пол — ТЕКСТУРА «неоновая схема» одним draw call (v0.3)
    (draw-mesh-tex *mesh-floor* *uv-floor* *tex-floor*
                   (mat-identity) mvp (list 1.0 1.0 1.0))
    ;; сетка сверху для свечения
    (draw-wire-lines *grid-wire* (mat-translate 0 0.06 0) mvp *grid-color*)
    ;; вращающийся тор в центре — ориентир
    (draw-mesh *mesh-torus* (mat4-mul (mat-translate -26 7 -20) (mat-rotate-y (* t 0.6)))
               mvp *torus-color*)
    ;; угловые пилоны — БАТЧ: 4 куба одним draw call (v0.3)
    (draw-batch *mesh-pillar* *pillar-insts* mvp)
    ;; редакторские пилоны (этап 6) — тем же батчем
    (if (null? (pylon-insts)) #f (draw-batch *mesh-pillar* (pylon-insts) mvp))
    ;; игрок — вращающийся куб с текстурой «панель» + тинт
    (draw-mesh-tex *mesh-cube* *uv-cube* *tex-panel*
                   (mat4-mul (mat4-mul (mat-translate pxv 1.4 pzv)
                                       (mat-scale 1.7 1.7 1.7))
                             (mat-rotate-y (* t 1.3)))
                   mvp *player-color*)
    ;; враги — БАТЧ + ТЕКСТУРА: все кубы арены одним draw call (v0.3)
    (draw-batch-tex *mesh-cube* *uv-cube* *tex-panel* (enemy-insts t) mvp)
    ;; пули — БАТЧ октаэдров одним draw call (v0.3)
    (draw-batch *mesh-octa* (bullet-insts t) mvp)
    ;; взрывы — растущие каркасы
    (for-each
     (lambda (fx)
       (let ((k (table-get fx 't 0.0)))
         (draw-wire-lines *fx-wire*
                          (mat4-mul (mat-translate (table-get fx 'x 0.0) 1.2 (table-get fx 'z 0.0))
                                    (mat-scale (+ 1.0 (* 11 k)) (+ 1.0 (* 11 k)) (+ 1.0 (* 11 k))))
                          mvp *fx-color*)))
     *fx*)))

(define (draw-hud)
  (draw-text (string-append "SCORE " (number->string *score*)) 6 5 1 (list 1.0 1.0 1.0))
  (draw-text (string-append "WAVE " (number->string *wave*)) 6 13 1 (list 0.65 0.72 0.9))
  (draw-text (string-append "KILLS " (number->string *kills*)) 80 13 1 (list 0.5 0.56 0.7))
  (draw-text (string-append "DRAW " (number->string (car (gpu3d-stats))))
             148 13 1 (list 0.5 0.56 0.7))
  (draw-text "ORBITAL" (- *screen-w* (text-width "ORBITAL" 1) 6) 5 1 (list 0.2 0.8 0.9)))

(define (draw-game-over)
  (draw-text "GAME OVER"
             (/ (- *screen-w* (text-width "GAME OVER" 3)) 2) 92 3 (list 1.0 0.3 0.3))
  (draw-text "PRESS R TO RESTART"
             (/ (- *screen-w* (text-width "PRESS R TO RESTART" 1)) 2) 132 1 (list 1.0 1.0 1.0))
  (draw-text (string-append "SCORE " (number->string *score*))
             (/ (- *screen-w* (text-width "SCORE 0000" 1)) 2) 146 1 (list 1.0 0.9 0.5)))

(define (draw)
  (let ((t (time3d)))
    (with-frame3d
     (draw-sky)
     (draw-scene t)
     (draw-hud)
     (if *game-over* (draw-game-over) #f))))

;; ——— мост редактора/MCP (v0.3 + этап 6): сцена и операции над сущностями ———
;; сущность по id: враг или пилон (id общие, из одного счётчика *next-id*)
(define (game-entity id)
  (let ((t (table-get *enemies* (number->string id) #f)))
    (if t t
        (let ((p (table-get *pylons* (number->string id) #f)))
          (if p p
              (error (string-append "нет сущности #" (number->string id))))))))

(define (game-remove! id)
  (table-remove! *enemies* (number->string id))
  (table-remove! *pylons* (number->string id)))

(define (game-spawn kind) (spawn-enemy kind))

(define (game-spawn-at kind x z)
  (if (eq? kind 'pylon)
      (spawn-pylon x z)
      (let ((id (spawn-enemy kind)))
        (let ((e (game-entity id)))
          (table-set! e 'x x)
          (table-set! e 'z z))
        id)))

;; сцена для иерархии редактора: таблицы → JSON-объекты (editor.lisp: obj)
(define (scene-entities)
  (append
   (list (obj 'id 0 'type "player" 'name "Игрок" 'ref "*player*"
              'x (table-get *player* 'x 0.0) 'z (table-get *player* 'z 0.0)))
   (map (lambda (entry)
          (let* ((id (string->number (car entry))) (e (cadr entry)))
            (obj 'id id 'type "enemy"
                 'name (symbol->string (table-get e 'kind 'grunt))
                 'ref (string-append "(game-entity " (number->string id) ")")
                 'x (table-get e 'x 0.0) 'z (table-get e 'z 0.0)
                 'hp (table-get e 'hp 1))))
        (table-entries *enemies*))
   (map (lambda (entry)
          (let* ((id (string->number (car entry))) (p (cadr entry)))
            (obj 'id id 'type "pylon" 'name "Пилон"
                 'ref (string-append "(game-entity " (number->string id) ")")
                 'x (table-get p 'x 0.0) 'z (table-get p 'z 0.0))))
        (table-entries *pylons*))
   (map (lambda (b)
          (obj 'type "bullet" 'name "Пуля"
               'x (table-get b 'x 0.0) 'z (table-get b 'z 0.0)))
        *bullets*)
   (map (lambda (fx)
          (obj 'type "fx" 'name "Взрыв"
               'x (table-get fx 'x 0.0) 'z (table-get fx 'z 0.0)))
        *fx*)))

;; ——— этап 6: камера для гизмо редактора (та же, что в draw-scene) ———
(define (editor-camera)
  (let ((pxv (px 0))
        (pzv (table-get *player* 'z 0.0))
        (t (time3d)))
    (obj 'eye (list (+ pxv (* 4 (sin (* t 0.25)))) 22 (+ pzv 27))
         'target (list pxv 0 (- pzv 5))
         'fov 1.0
         'w *screen-w* 'h *screen-h*)))

;; ——— этап 6: префабы для drag-n-drop редактора ———
(define (prefab-list)
  (list (obj 'kind "grunt" 'name "Грант" 'hp 1)
        (obj 'kind "brute" 'name "Брут" 'hp 3)
        (obj 'kind "pylon" 'name "Пилон")))
