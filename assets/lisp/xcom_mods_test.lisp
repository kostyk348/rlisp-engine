;; xcom_mods_test.lisp — мод меняет мир: до/после, механика тоже переписывается.
(load "assets/lisp/core.lisp")
(load "assets/lisp/xcom_rules.lisp")
(load "assets/lisp/mods.lisp")

(define *passed* 0)
(define *failed* 0)
(define (check name condition detail)
  (if condition
      (begin (set! *passed* (+ *passed* 1)) (print (string-append "PASS " name " | " detail)))
      (begin (set! *failed* (+ *failed* 1)) (print (string-append "FAIL " name " | " detail)))))

(define before (damage-range (rule 'weapon "pistol")))
(check "до мода пистолет 2..5" (equal? before (list 2 5))
       (string-append (number->string (car before)) ".."
                      (number->string (car (cdr before)))))

(check "до мода сектоида-ветерана нет" (not (rule 'alien "sectoid-veteran")) "запись отсутствует")

(load-mods)

(define after (damage-range (rule 'weapon "pistol")))
(check "после мода пистолет 4..7" (equal? after (list 4 7))
       (string-append (number->string (car after)) ".."
                      (number->string (car (cdr after)))))

(define veteran (rule 'alien "sectoid-veteran"))
(check "сектоид-ветеран появился" (if veteran #t #f) (table-get veteran 'name))
(check "у ветерана hp 9" (= (table-get veteran 'hp) 9) (number->string (table-get veteran 'hp)))

(check "мод переписал механику" (= (alien-accuracy 3) 33)
       (string-append "пришелец на 3 тайла: " (number->string (alien-accuracy 3)) "%"))

(print (string-append "ИТОГ: " (number->string *passed*) " PASS, " (number->string *failed*) " FAIL"))
