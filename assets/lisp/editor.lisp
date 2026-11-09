;; editor.lisp — Уровень 3: мост редактора и MCP (v0.3).
;; Редактор (--serve) и ИИ-агент (--mcp) работают со сценой ТОЛЬКО через Lisp:
;;   (editor-scene)  — сцена как список таблиц (JSON-объекты) для иерархии;
;;   (mcp-spawn …)   — спавн сущности;
;;   (mcp-set …)     — правка свойства живой сущности;
;;   (mcp-remove …)  — удаление.
;; Игра регистрирует хуки (scene-entities / game-spawn / game-entity /
;; game-remove!) — у 2D и 3D игр они свои. Нет хука — инструмент честно
;; отвечает, что в этой игре его нет.

(define (editor-scene)
  (if (defined? 'scene-entities)
      (scene-entities)
      '()))

(define (mcp-spawn kind)
  (if (defined? 'game-spawn)
      (game-spawn kind)
      (error "в этой игре нет game-spawn")))

(define (mcp-spawn-at kind x z)
  (if (defined? 'game-spawn-at)
      (game-spawn-at kind x z)
      (mcp-spawn kind)))

(define (mcp-entity id)
  (if (defined? 'game-entity)
      (game-entity id)
      (error "в этой игре нет game-entity")))

;; свойство сущности: (mcp-set 3 'x 12.5)
(define (mcp-set id prop val)
  (table-set! (mcp-entity id) prop val))

(define (mcp-remove id)
  (if (defined? 'game-remove!)
      (game-remove! id)
      (error "в этой игре нет game-remove!")))

;; хелпер: таблица-объект из плоского списка «ключ значение …»
(define (obj . kvs)
  (let ((t (make-table)))
    (set-kv! t kvs)
    t))

;; ——— этап 6: сохранение сцены в Lisp-файл ———
;; Живая сцена (scene-entities) → формы восстановления:
;;   enemy / pylon → (mcp-spawn-at 'kind x z);
;;   player        → (begin (table-set! REF 'x X) (table-set! REF 'z Z));
;;   прочее (пули, взрывы) — переходящее, не сериализуется.
;; Вызов: (editor-save-scene "scene_saved.lisp") — пишет нативом file-write.
;; Возврат в игру: (load "scene_saved.lisp") через REPL редактора.

;; соединить строки через разделитель (dialect без named let — рекурсия)
(define (str-join sep lst)
  (if (null? lst) ""
      (if (null? (cdr lst))
          (car lst)
          (string-append (car lst) sep (str-join sep (cdr lst))))))

;; сцена → одна форма восстановления (или "" для переходящих сущностей)
(define (scene->form e)
  (let ((t (table-get e 'type "?"))
        (x (number->string (table-get e 'x 0.0)))
        (z (number->string (table-get e 'z 0.0))))
    (if (string=? t "player")
        (string-append "(begin (table-set! " (table-get e 'ref "*player*")
                       " 'x " x ") (table-set! " (table-get e 'ref "*player*")
                       " 'z " z "))")
        (if (string=? t "pylon")
            (string-append "(mcp-spawn-at 'pylon " x " " z ")")
            (if (string=? t "enemy")
                (string-append "(mcp-spawn-at '"
                               (table-get e 'name "grunt") " " x " " z ")")
                "")))))

(define (editor-save-scene fname)
  (if (defined? 'scene-entities)
      (let* ((forms (map scene->form (editor-scene)))
             (kept (filter (lambda (s) (> (string-length s) 0)) forms))
             (src (string-append
                   ";; scene_saved.lisp — сцена, сохранённая редактором rlisp-engine.\n"
                   ";; Восстановление в живой игре: (load \"scene_saved.lisp\") через REPL.\n"
                   (str-join "\n" kept))))
        (file-write fname src)
        (length kept))
      (error "в этой игре нет scene-entities — сохранять нечего")))
