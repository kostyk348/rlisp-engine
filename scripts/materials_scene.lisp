;; materials_scene.lisp — визуальное доказательство этапа 7 «материалы».
;; Одна сцена (земля + 100 кустов-билбордов) рендерится 4 раза в один прогон:
;;   1-base  — старый Lambert (все фичи выкл)
;;   2-alpha — + alpha-test (cutout: сквозь прорези травинок видно «небо»)
;;   3-spec  — + блик Blinn-Phong
;;   4-full  — весь материал через (use-material! …): тайлинг + cutout +
;;             блик + туман (Lisp-слой materials.lisp)
;; Кадры пишутся в /tmp/stage7-proof/*.png; сверка пикселей и коллаж —
;; в scripts/materials_proof.sh (PIL).
(load "assets/lisp/core.lisp")
(load "assets/lisp/render3d.lisp")
(load "assets/lisp/textures.lisp")
(load "assets/lisp/materials.lisp")
(window-create 320 240 "t")

;; трава: зелёный шум + диагональные прорези alpha=0 (для alpha-test)
(define W 64)
(define grass (tex-canvas W W))
(dotimes (y W)
  (dotimes (x W)
    (let ((i (* 4 (+ (* y W) x)))
          (streak (+ 0.30 (* 0.14 (/ (mod (* x 7) 13) 13.0))))
          (noise (* 0.08 (/ (mod (+ (* x 11) (* y 17)) 7) 7.0))))
      (f32set! grass i (+ 0.10 streak noise))
      (f32set! grass (+ i 1) (+ 0.34 streak noise))
      (f32set! grass (+ i 2) (* 0.30 (+ 0.10 streak noise)))
      (f32set! grass (+ i 3) (if (< (mod (+ x y) 9) 2) 0.0 1.0)))))
(define grass-id (texture-upload W W grass))

;; земля: тёмная почва с лёгкой пятнистостью
(define W2 32)
(define soil (tex-canvas W2 W2))
(dotimes (y W2)
  (dotimes (x W2)
    (let ((i (* 4 (+ (* y W2) x)))
          (v (+ 0.16 (* 0.10 (/ (mod (+ (* x 5) (* y 9)) 11) 11.0)))))
      (f32set! soil i (* 0.55 v))
      (f32set! soil (+ i 1) v)
      (f32set! soil (+ i 2) (* 0.45 v)))))
(define soil-id (texture-upload W2 W2 soil))

;; реестр материалов (уровень 3, materials.lisp)
(defmaterial 'земля ':texture soil-id ':tiling '(24 24) ':spec 0.15)
(defmaterial 'кусты ':texture grass-id ':alpha-test 0.4 ':spec 0.6 ':shininess 48)

;; геометрия и камера
(define mvp (camera-mvp (list 0 2 28) (list 0 0 0) 0.9))
(define bpos (list -1 -1 0  1 -1 0  1 1 0  -1 1 0))
(define buv (list 0 0  1 0  1 1  0 1))
(define bidx (list 0 1 2  0 2 3))

;; инстансы кустов: сетка 10×10, лёгкий случайный yaw, глубина ступенями
(define (строка gy gx acc)
  (if (= gx 10) acc
      (строка gy (+ gx 1)
               (append acc (list (* (- gx 4.5) 5.0)
                                 (+ (* (- gy 4.5) 3.8) 1.0)
                                 (* (- gy) 2.2)
                                 6 7 6
                                 0.0
                                 (* 0.15 (- (/ (mod (+ (* gx 3) (* gy 5)) 7) 7.0) 0.5))
                                 0.0
                                 0.75 1.0 0.6)))))
(define (сетка gy acc)
  (if (= gy 10) acc (сетка (+ gy 1) (строка gy 0 acc))))
;; кусты — СПИСОК (draw-batch-tex сам делает f32-from-list)
(define bushes (сетка 0 '()))

(define ground-mesh (mesh-plane 40 8))
(define ground-uv (mesh-plane-uv 8))

;; один кадр сцены: земля + кусты (текстуры передаются явно; материал —
;; опциональным аргументом draw-хелпера)
(define (кадр mat?)
  (gpu3d-clear 0.55 0.7 0.9)
  (if mat?
      (draw-mesh-tex ground-mesh ground-uv soil-id (mat-identity) mvp (list 1 1 1) 'земля)
      (draw-mesh-tex ground-mesh ground-uv soil-id (mat-identity) mvp (list 1 1 1)))
  (if mat?
      (draw-batch-tex (list bpos bidx) buv grass-id bushes mvp 'кусты)
      (draw-batch-tex (list bpos bidx) buv grass-id bushes mvp)))

;; ——— 1: база — все фичи выключены (старый Lambert) ———
(gpu3d-alpha-test 0)
(gpu3d-light 0.3 0.7 0.65 0.35 0.75)
(gpu3d-view 0 2 28)
(gpu3d-fog 0.55 0.7 0.9 0)
(gpu3d-mat-tiling 1 1)
(кадр #f)
(gpu-screenshot "/tmp/stage7-proof/1-base.png")

;; ——— 2: + alpha-test (cutout) ———
(gpu3d-alpha-test 0.4)
(кадр #f)
(gpu-screenshot "/tmp/stage7-proof/2-alpha.png")

;; ——— 3: + блик Blinn-Phong ———
(gpu3d-alpha-test 0)
(gpu3d-light 0.3 0.7 0.65 0.35 0.75 0.6 48)
(кадр #f)
(gpu-screenshot "/tmp/stage7-proof/3-spec.png")

;; ——— 4: весь материал через use-material! (тайлинг + cutout + блик + туман) ———
(gpu3d-light 0.3 0.7 0.65 0.35 0.75)
(gpu3d-fog 0.55 0.7 0.9 0.045)
(кадр #t)
(gpu-screenshot "/tmp/stage7-proof/4-full.png")
"готово"
