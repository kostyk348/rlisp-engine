;; main3d.lisp — точка входа 3D-стека (v0.2).
;; Запуск: rlisp-engine --script assets/lisp/main3d.lisp
;; Rust-ядро грузит ЭТОТ файл; 2D-стек (main.lisp) не затронут.

(load "core.lisp")       ;; базовая библиотека + defpersistent
(load "font.lisp")       ;; данные: растровый шрифт (+ глифы v0.2)
(load "graphics.lisp")   ;; системный DSL: кадр, текст, примитивы 2D (HUD)
(load "input.lisp")      ;; системный DSL: состояние клавиш
(load "render3d.lisp")   ;; системный DSL v0.2: матрицы, камера, меши, draw (+батч v0.3)
(load "textures.lisp")   ;; системный DSL v0.3: процедурные текстуры, UV, спрайты
(load "materials.lisp")  ;; системный DSL этапа 7: defmaterial/use-material! (alpha-test, блик, тайлинг)
(load "editor.lisp")     ;; системный DSL v0.3: мост редактора и MCP (scene/spawn/set)
(load "game3d.lisp")     ;; уровень 4: 3D-игра «ORBITAL»

(print "rlisp-engine 3D: HAL(+GPU) → интерпретатор → render3d DSL → ORBITAL; update/draw на Lisp")
