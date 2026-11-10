;; main_physpoly.lisp — точка входа «ФИЗПОЛИГОН» (этап 10).
;; Запуск: rlisp-engine --script assets/lisp/main_physpoly.lisp
;; Минимум системных DSL: без текстур/материалов/glTF (вершинные цвета) —
;; тот же набор работает в wasm-песочнице.

(load "core.lisp")
(load "textures.lisp")  ;; этап 20: tex-canvas/tex-px! для облаков       ;; базовая библиотека + defpersistent
(load "font.lisp")       ;; данные: растровый шрифт
(load "graphics.lisp")   ;; системный DSL: кадр, текст, примитивы 2D (HUD)
(load "input.lisp")      ;; системный DSL: состояние клавиш
(load "render3d.lisp")   ;; системный DSL: матрицы, камера, меши, draw (+батч)
(load "phys_poly.lisp")  ;; сцена «ФИЗПОЛИГОН» (этап 10)

(print "rlisp-engine PHYS POLY: phys-* нативы → сферы/heightfield/вода; update/draw на Lisp")
