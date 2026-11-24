;; mods.lisp — глобальные модификации: один файл = изменение мира.
;;
;; Мод — обычный Lisp-файл: он может менять правила, добавлять оружие и
;; пришельцев, подменять функции. Порядок загрузки — порядок применения,
;; значит «кто позже, тот и решает» (как в OpenXcom приоритеты модов).
;;
;;   (load "assets/lisp/mods.lisp")  ; после xcom_rules.lisp
;;   (load-mods)

(define *mods*
  (list
    "assets/lisp/mods/sample_global.lisp"))

(define (load-mods)
  (for-each (lambda (path)
              (print (string-append "мод: " path))
              (load path))
            *mods*))
