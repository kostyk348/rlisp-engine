;; xcom.lisp — тактический срез X-COM на rlisp-engine.
;; Не порт OpenXcom (~200k строк C++: геоскейп, базы, исследования, НЛО),
;; а боевое ядро, с которого порт начинается: сетка, отряд, единицы времени (TU),
;; ход/стрельба с шансом попадания, ход пришельцев, детерминизм по сиду.
;;
;;   ./target/release/rlisp-engine --script assets/lisp/xcom.lisp --ascii --frames 300

;; core.lisp даёт defpersistent и прочую «Lisp-ОС» основу.
(load "assets/lisp/core.lisp")
(load "assets/lisp/input.lisp")
(load "assets/lisp/graphics.lisp")
(load "assets/lisp/font.lisp")

(window-create 640 480 "X-COM :: rlisp-engine")

; в ядре нет fold-left — определяем сами (обычный Lisp, как и всё остальное)
(define (fold-left f init lst)
  (if (null? lst) init (fold-left f (f init (car lst)) (cdr lst))))

(define *tile* 24)
(define *cols* 16)
(define *rows* 12)
(define *turn* 0)
(define *message* "СТРЕЛКИ КУРСОР / M ИДТИ / F ОГОНЬ / E КОНЕЦ ХОДА / TAB БОЕЦ")
(define *game-over* #f)

(defpersistent *cursor-x* 1)
(defpersistent *cursor-y* 1)
(defpersistent *selected* 0)

(define (make-unit name x y tu hp friendly)
  (let ((u (make-table)))
    (table-set! u 'name name)
    (table-set! u 'x x)
    (table-set! u 'y y)
    (table-set! u 'tu tu)
    (table-set! u 'hp hp)
    (table-set! u 'friendly friendly)
    u))

(defpersistent *units*
  (list (make-unit "Боец-1" 1 8 60 10 #t)
        (make-unit "Боец-2" 2 9 60 10 #t)
        (make-unit "Сектоид-1" 12 2 60 6 #f)
        (make-unit "Сектоид-2" 13 5 60 6 #f)))

(define (alive? u) (> (table-get u 'hp) 0))
(define (ux u) (table-get u 'x))
(define (uy u) (table-get u 'y))
(define (friendly? u) (table-get u 'friendly))

(define (manhattan ax ay bx by)
  (+ (if (> ax bx) (- ax bx) (- bx ax))
     (if (> ay by) (- ay by) (- by ay))))

(define (distance a b) (manhattan (ux a) (uy a) (ux b) (uy b)))

(define (unit-at x y)
  (fold-left (lambda (found u)
               (if (and (alive? u) (= (ux u) x) (= (uy u) y)) u found))
             #f *units*))

;; Возвращаем ЮНИТА (без cons — improper pair в этом ядре вёл себя непредсказуемо).
;; Враг — юнит с ПРОТИВОПОЛОЖНЫМ флагом: для бойца пришелец, для пришельца боец.
(define (enemy-of? u e)
  (if (friendly? u) (not (friendly? e)) (friendly? e)))

(define (nearest-enemy u)
  (fold-left (lambda (best e)
               (if (and (alive? e) (enemy-of? u e)
                        (not (and (= (ux e) (ux u)) (= (uy e) (uy u)))))
                   (if (or (not best) (< (distance u e) (distance u best))) e best)
                   best))
             #f *units*))

(define (hit-chance d)
  (let ((c (- 70 (* 6 d))))
    (if (< c 10) 10 (if (> c 90) 90 c))))

(define (roll-hit shooter target)
  (let ((chance (hit-chance (distance shooter target)))
        (roll (random-int 1 100)))
    (if (<= roll chance)
        (let ((damage (random-int 2 5)))
          (table-set! target 'hp (- (table-get target 'hp) damage))
          (string-append (table-get shooter 'name) " → " (table-get target 'name)
                         ": шанс " (number->string chance) "%, бросок " (number->string roll)
                         ", ПОПАДАНИЕ на " (number->string damage) ", у цели HP "
                         (number->string (table-get target 'hp))))
        (string-append (table-get shooter 'name) " → " (table-get target 'name)
                       ": шанс " (number->string chance) "%, бросок " (number->string roll)
                       ", промах"))))

(define (fire shooter target)
  (if (< (table-get shooter 'tu) 25)
      (set! *message* "нет TU на выстрел (нужно 25)")
      (begin
        (table-set! shooter 'tu (- (table-get shooter 'tu) 25))
        (set! *message* (roll-hit shooter target)))))

(define (alien-step u)
  (table-set! u 'tu 60)
  (let ((target (nearest-enemy u)))
    (if target
        (let ((gap (distance u target)))
          (if (<= gap 2)
              (fire u target)
              (let ((nx (if (< (ux u) (ux target))
                            (+ (ux u) 1)
                            (if (> (ux u) (ux target)) (- (ux u) 1) (ux u))))
                    (ny (if (< (uy u) (uy target))
                            (+ (uy u) 1)
                            (if (> (uy u) (uy target)) (- (uy u) 1) (uy u)))))
                (if (not (unit-at nx ny))
                    (begin (table-set! u 'x nx) (table-set! u 'y ny)))))))))

(define (alien-turn)
  (for-each (lambda (u) (if (and (alive? u) (not (friendly? u))) (alien-step u))) *units*))

(define (count-alive want-friendly)
  (fold-left (lambda (n u)
               (if (and (alive? u) (equal? (friendly? u) want-friendly)) (+ n 1) n))
             0 *units*))

(define (check-outcome)
  (if (= (count-alive #f) 0)
      (begin (set! *game-over* #t) (set! *message* "ПОБЕДА: все пришельцы мертвы")))
  (if (= (count-alive #t) 0)
      (begin (set! *game-over* #t) (set! *message* "ПОРАЖЕНИЕ: отряд уничтожен"))))

(define (selected-unit) (nth *units* *selected*))

(define (move-selected)
  (let* ((u (selected-unit))
         (cost (* 4 (manhattan (ux u) (uy u) *cursor-x* *cursor-y*)))
         (blocked (unit-at *cursor-x* *cursor-y*)))
    (if (not (and (alive? u) (friendly? u)))
        (set! *message* "выбран не боец — TAB")
        (if blocked
            (set! *message* "клетка занята")
            (if (> cost (table-get u 'tu))
                (set! *message* "не хватает TU")
                (begin
                  (table-set! u 'tu (- (table-get u 'tu) cost))
                  (table-set! u 'x *cursor-x*)
                  (table-set! u 'y *cursor-y*)
                  (set! *message* (string-append (table-get u 'name) " идёт: -"
                                                 (number->string cost) " TU, осталось "
                                                 (number->string (table-get u 'tu))))))))))

(define (handle-input)
  (if (key-pressed? "Left") (if (> *cursor-x* 0) (set! *cursor-x* (- *cursor-x* 1))))
  (if (key-pressed? "Right") (if (< *cursor-x* (- *cols* 1)) (set! *cursor-x* (+ *cursor-x* 1))))
  (if (key-pressed? "Up") (if (> *cursor-y* 0) (set! *cursor-y* (- *cursor-y* 1))))
  (if (key-pressed? "Down") (if (< *cursor-y* (- *rows* 1)) (set! *cursor-y* (+ *cursor-y* 1))))
  (if (key-pressed? "Tab")
      (set! *selected* (if (< *selected* (- (length *units*) 1)) (+ *selected* 1) 0)))
  (if (key-pressed? "m") (move-selected))
  (if (key-pressed? "f")
      (let ((target (unit-at *cursor-x* *cursor-y*)))
        (if (and target (not (friendly? target)))
            (fire (selected-unit) target)
            (set! *message* "под курсором нет пришельца"))))
  (if (key-pressed? "e")
      (begin
        (for-each (lambda (u) (if (and (alive? u) (friendly? u)) (table-set! u 'tu 60))) *units*)
        (set! *turn* (+ *turn* 1))
        (alien-turn)
        (check-outcome))))

(define (draw-tile col row)
  (if (= (mod (+ col row) 2) 0)
      (gpu-fill-rect (* col *tile*) (* row *tile*) *tile* *tile* 0.16 0.20 0.22)
      (gpu-fill-rect (* col *tile*) (* row *tile*) *tile* *tile* 0.20 0.25 0.27)))

(define (draw-grid)
  (for-each (lambda (row)
              (for-each (lambda (col) (draw-tile col row)) (range 0 *cols*)))
            (range 0 *rows*)))

(define (draw-unit u)
  (if (alive? u)
      (let ((x (* (ux u) *tile*)) (y (* (uy u) *tile*)))
        (if (friendly? u)
            (gpu-fill-rect (+ x 4) (+ y 4) (- *tile* 8) (- *tile* 8) 0.25 0.55 0.95)
            (gpu-fill-rect (+ x 4) (+ y 4) (- *tile* 8) (- *tile* 8) 0.85 0.25 0.25)))))

(define (draw-units) (for-each draw-unit *units*))

(define (draw-cursor)
  (let ((x (* *cursor-x* *tile*)) (y (* *cursor-y* *tile*)))
    (gpu-fill-rect x y *tile* 3 1.0 0.9 0.2)
    (gpu-fill-rect x (+ y (- *tile* 3)) *tile* 3 1.0 0.9 0.2)
    (gpu-fill-rect x y 3 *tile* 1.0 0.9 0.2)
    (gpu-fill-rect (+ x (- *tile* 3)) y 3 *tile* 1.0 0.9 0.2)))

(define (draw-hud)
  (let ((u (selected-unit))
        (y (+ (* *rows* *tile*) 6)))
    (draw-text (string-append "ХОД " (number->string *turn*)) 6 y 1 (list 1.0 1.0 1.0))
    (draw-text (string-append (table-get u 'name) "  TU " (number->string (table-get u 'tu))
                              "  HP " (number->string (table-get u 'hp))
                              "  врагов " (number->string (count-alive #f)))
               6 (+ y 12) 1 (list 0.6 0.8 1.0))
    (draw-text *message* 6 (+ y 26) 1 (list 0.85 0.85 0.6))))

(define (draw)
  (with-frame
   (gpu-fill-rect 0 0 640 480 0.08 0.09 0.10)
   (draw-grid)
   (draw-units)
   (draw-cursor)
   (draw-hud)))

(define (update dt)
  (if (not *game-over*) (handle-input)))

(random-seed 42)
