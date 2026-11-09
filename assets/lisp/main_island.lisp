;; main_island.lisp — точка входа демо «ОСТРОВ» (этап 8).
;; Запуск: rlisp-engine --script assets/lisp/main_island.lisp
;; Отдельный скрипт: main3d.lisp (ORBITAL) и main.lisp (2D) не тронуты.

(load "core.lisp")       ;; базовая библиотека + defpersistent
(load "font.lisp")       ;; данные: растровый шрифт
(load "graphics.lisp")   ;; системный DSL: кадр, текст, примитивы 2D (HUD)
(load "input.lisp")      ;; системный DSL: состояние клавиш
(load "render3d.lisp")   ;; системный DSL: матрицы, камера, меши, draw (+батч)
(load "textures.lisp")   ;; системный DSL: UV-развёртки примитивов
(load "materials.lisp")  ;; системный DSL этапа 7: defmaterial/use-material!
(load "load-gltf.lisp")  ;; уровень 3 этапа 8: glTF 2.0 (ASCII) загрузчик
(load "island.lisp")     ;; уровень 4: сцена «ОСТРОВ»

(print "rlisp-engine ISLAND: HAL(+GPU) → render3d → материалы → glTF → сцена; update/draw на Lisp")
