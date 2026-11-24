;; xcom_rules.lisp — рулсет X-COM как Lisp-данные.
;;
;; В OpenXcom правила лежат в YAML и грузятся в C++-структуры. У нас правила —
;; это Lisp: их можно править на лету (hot reload), проверять тестами и
;; расширять модами без пересборки. Каждое правило — таблица с ключами.
;;
;;   (load "assets/lisp/xcom_rules.lisp")
;;   (rule 'weapon "pistol")        → таблица оружия
;;   (damage-range (rule 'weapon "pistol")) → (2 5)

(define *rules* (make-table))

(define (rule-category name) (table-get *rules* name))

(define (register! category entry)
  (let ((bucket (rule-category category)))
    (if (not bucket)
        (begin (set! bucket (make-table)) (table-set! *rules* category bucket)))
    (table-set! bucket (table-get entry 'id) entry)
    entry))

(define (rule category id)
  (let ((bucket (rule-category category)))
    (if bucket (table-get bucket id) #f)))

(define (rules-in category)
  (let ((bucket (rule-category category)))
    (if bucket (table-keys bucket) '())))

;; ——— конструктор правила: (mkrule "pistol" '(name "Пистолет" tu 25)) ———
;; Макросы с `,@` в этом ядре не сплайсятся — обходимся функциями.
(define (pairs lst)
  (if (null? lst) '()
      (cons (list (car lst) (car (cdr lst))) (pairs (cdr (cdr lst))))))

(define (mkrule id kvs)
  (let ((entry (make-table)))
    (table-set! entry 'id id)
    (for-each (lambda (pair)
                (table-set! entry (car pair) (car (cdr pair))))
              (pairs kvs))
    entry))

;; ——— данные: оружие (по мотивам OpenXcom) ———
(register! 'weapon (mkrule "pistol" '(name "Пистолет" tu 25 damage-min 2 damage-max 5 range 12 accuracy 10)))
(register! 'weapon (mkrule "rifle" '(name "Винтовка" tu 30 damage-min 3 damage-max 6 range 20 accuracy 0)))
(register! 'weapon (mkrule "heavy-cannon" '(name "Тяжёлая пушка" tu 45 damage-min 6 damage-max 12 range 30 accuracy -20)))
(register! 'weapon (mkrule "grenade" '(name "Граната" tu 25 damage-min 4 damage-max 9 range 6 accuracy 0)))
(register! 'weapon (mkrule "stun-rod" '(name "Электрошокер" tu 20 damage-min 0 damage-max 2 range 1 accuracy 20)))

;; ——— данные: пришельцы ———
(register! 'alien (mkrule "sectoid" '(name "Сектоид" hp 6 tu 60 psi-skill 20 armor 0)))
(register! 'alien (mkrule "floater" '(name "Плаван" hp 8 tu 65 psi-skill 0 armor 1)))
(register! 'alien (mkrule "muton" '(name "Мутон" hp 12 tu 60 psi-skill 0 armor 2)))
(register! 'alien (mkrule "chryssalid" '(name "Хризалида" hp 14 tu 90 psi-skill 0 armor 1)))

;; ——— данные: классы бойцов (в OpenXcom их называли бы armor/ruleset) ———
(register! 'soldier-class (mkrule "rookie" '(name "Новобранец" hp 10 tu 60 accuracy 60 armor 1)))
(register! 'soldier-class (mkrule "veteran" '(name "Ветеран" hp 14 tu 65 accuracy 70 armor 2)))
(register! 'soldier-class (mkrule "elite" '(name "Элита" hp 18 tu 70 accuracy 80 armor 3)))

;; ——— производные правила поверх данных ———
(define (damage-range weapon)
  (list (table-get weapon 'damage-min) (table-get weapon 'damage-max)))

(define (effective-accuracy weapon-class accuracy-bonus distance)
  (let ((weapon (rule 'weapon weapon-class)))
    (let ((base (+ (table-get weapon 'accuracy) accuracy-bonus)))
      (let ((chance (- base (* 6 distance))))
        (if (< chance 10) 10 (if (> chance 90) 90 chance))))))

(define (roll-damage weapon)
  (random-int (table-get weapon 'damage-min) (table-get weapon 'damage-max)))

;; ——— валидация: правило, ссылающееся на несуществующее, — ошибка, а не тишина ———
(define (validate-rules)
  (define problems '())
  (define (problem! text) (set! problems (cons text problems)))
  (for-each
   (lambda (id)
     (let ((weapon (rule 'weapon id)))
       (if (not (table-get weapon 'name)) (problem! (string-append "weapon " id ": нет name")))
       (if (> (table-get weapon 'damage-min) (table-get weapon 'damage-max))
           (problem! (string-append "weapon " id ": damage-min > damage-max")))
       (if (<= (table-get weapon 'range) 0) (problem! (string-append "weapon " id ": range <= 0")))))
   (rules-in 'weapon))
  (for-each
   (lambda (id)
     (let ((alien (rule 'alien id)))
       (if (<= (table-get alien 'hp) 0) (problem! (string-append "alien " id ": hp <= 0")))
       (if (< (table-get alien 'tu) 10) (problem! (string-append "alien " id ": tu < 10")))))
   (rules-in 'alien))
  (for-each
   (lambda (id)
     (let ((class (rule 'soldier-class id)))
       (if (> (table-get class 'accuracy) 100) (problem! (string-append "class " id ": accuracy > 100")))))
   (rules-in 'soldier-class))
  problems)
