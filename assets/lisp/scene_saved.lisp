;; scene_saved.lisp — сцена, сохранённая редактором rlisp-engine.
;; Восстановление в живой игре: (load "scene_saved.lisp") через REPL.
(begin (table-set! *player* 'x 0.0) (table-set! *player* 'z 0.0))
(mcp-spawn-at 'brute -16.142481 -5.165594)
(mcp-spawn-at 'pylon 18 -20)
(mcp-spawn-at 'pylon -20 -18)