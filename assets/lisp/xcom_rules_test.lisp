;; xcom_rules_test.lisp — правила как данные: проверка загрузки, производных и валидации.
(load "assets/lisp/core.lisp")
(load "assets/lisp/xcom_rules.lisp")

(define *passed* 0)
(define *failed* 0)
(define (check name condition detail)
  (if condition
      (begin (set! *passed* (+ *passed* 1)) (print (string-append "PASS " name " | " detail)))
      (begin (set! *failed* (+ *failed* 1)) (print (string-append "FAIL " name " | " detail)))))

(check "оружия загружено" (= (length (rules-in 'weapon)) 5)
       (string-append "штук " (number->string (length (rules-in 'weapon)))))
(check "пришельцев загружено" (= (length (rules-in 'alien)) 4)
       (string-append "штук " (number->string (length (rules-in 'alien)))))
(check "классов бойцов" (= (length (rules-in 'soldier-class)) 3)
       (string-append "штук " (number->string (length (rules-in 'soldier-class)))))

(define pistol (rule 'weapon "pistol"))
(check "пистолет найден" (if pistol #t #f) (table-get pistol 'name))
(check "урон пистолета 2..5" (equal? (damage-range pistol) (list 2 5))
       (string-append (number->string (car (damage-range pistol))) ".."
                      (number->string (car (cdr (damage-range pistol))))))

(check "точность падает с дистанцией" (< (effective-accuracy "pistol" 0 8) (effective-accuracy "pistol" 0 1))
       (string-append (number->string (effective-accuracy "pistol" 0 1)) "% → "
                      (number->string (effective-accuracy "pistol" 0 8)) "%"))
(check "точность не ниже 10%" (= (effective-accuracy "heavy-cannon" 0 20) 10)
       (string-append (number->string (effective-accuracy "heavy-cannon" 0 20)) "%"))

(define dmg (roll-damage pistol))
(check "урон в диапазоне" (and (>= dmg 2) (<= dmg 5)) (number->string dmg))

(check "валидация текущих данных чиста" (= (length (validate-rules)) 0)
       (string-append "проблем " (number->string (length (validate-rules)))))

;; ломаем правило намеренно — валидация обязана это поймать
(register! 'weapon (mkrule "broken" '(name "Сломанный" tu 10 damage-min 9 damage-max 1 range 0 accuracy 0)))
(check "валидация ловит кривое правило" (> (length (validate-rules)) 0)
       (string-append "проблем " (number->string (length (validate-rules)))))

(print (string-append "ИТОГ: " (number->string *passed*) " PASS, " (number->string *failed*) " FAIL"))
