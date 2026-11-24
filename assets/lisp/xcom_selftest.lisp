;; xcom_selftest.lisp — доказательство правил боя без окна и без игрока.
;;   ./target/release/rlisp-engine --script assets/lisp/xcom_selftest.lisp --ascii --frames 1
(load "assets/lisp/xcom.lisp")

(define *passed* 0)
(define *failed* 0)

(define (check name condition detail)
  (if condition
      (begin (set! *passed* (+ *passed* 1))
             (print (string-append "PASS " name " | " detail)))
      (begin (set! *failed* (+ *failed* 1))
             (print (string-append "FAIL " name " | " detail)))))

;; 1) шанс попадания: убывает с расстоянием и не выходит за 10..90
(check "hit-chance дистанция 0" (= (hit-chance 0) 70) (number->string (hit-chance 0)))
(check "hit-chance дистанция 10" (= (hit-chance 10) 10) (number->string (hit-chance 10)))
(check "hit-chance потолок" (= (hit-chance 0) 70) "не выше 90 при отрицательной дистанции не бывает")

;; 2) выстрел тратит TU и наносит урон только при попадании
(define shooter (make-unit "Т" 0 0 60 10 #t))
(define target (make-unit "Ц" 1 0 60 10 #f))
(define hits 0)
(define misses 0)
(define (loop-shots n)
  (if (> n 0)
      (begin
        (table-set! shooter 'tu 60)
        (table-set! target 'hp 10)
        (roll-hit shooter target)
        (if (< (table-get target 'hp) 10) (set! hits (+ hits 1)) (set! misses (+ misses 1)))
        (loop-shots (- n 1)))))
(loop-shots 200)
(check "попадания происходят" (> hits 0) (string-append "попаданий " (number->string hits)
                                                          " из 200, промахов " (number->string misses)))
(check "промахи происходят" (> misses 0) (string-append "промахов " (number->string misses)))
(check "шанс ≈70% на дистанции 1" (and (> hits 100) (< hits 180))
       (string-append "попаданий " (number->string hits) " из 200"))

;; 3) fire списывает ровно 25 TU
(table-set! shooter 'tu 60)
(fire shooter target)
(check "fire списал 25 TU" (= (table-get shooter 'tu) 35)
       (string-append "TU " (number->string (table-get shooter 'tu))))
(table-set! shooter 'tu 10)
(fire shooter target)
(check "нет TU — нет выстрела" (= (table-get shooter 'tu) 10)
       (string-append "TU " (number->string (table-get shooter 'tu))))

;; 4) ход пришельцев: пришелец приближается к ближайшему бойцу
(define alien (make-unit "П" 10 5 60 6 #f))
(define soldier (make-unit "Б" 1 5 60 10 #t))
(set! *units* (list soldier alien))
(define before (distance alien soldier))
(alien-step alien)
(define after (distance alien soldier))
(check "пришелец идёт к бойцу" (< after before)
       (string-append "было " (number->string before) " стало " (number->string after)))

;; 5) пришелец рядом стреляет вместо шага
(set! *units* (list (make-unit "Б2" 1 1 60 10 #t) (make-unit "П2" 2 1 60 6 #f)))
(alien-step (nth *units* 1))
(check "вблизи пришелец стреляет" (= (table-get (nth *units* 1) 'x) 2) "не сдвинулся вплотную")

;; 6) исход боя
(set! *units* (list (make-unit "Б3" 1 1 60 10 #t) (make-unit "П3" 2 1 60 0 #f)))
(check-outcome)
(check "победа при нуле пришельцев" *game-over* "game-over выставлен")

(print (string-append "ИТОГ: " (number->string *passed*) " PASS, "
                      (number->string *failed*) " FAIL"))
