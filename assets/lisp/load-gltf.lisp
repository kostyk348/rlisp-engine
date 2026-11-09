;; load-gltf.lisp — Уровень 3: минимальный glTF 2.0 загрузчик (этап 8).
;; Модели из интернета → F32Vec-вершины/UV/индексы + PNG-текстура →
;; существующие gpu3d-mesh-tex / draw-gltf. Данные: внешний .bin (натив
;; file-read-bytes — file-read требует валидный UTF-8) ИЛИ встроенный
;; data-URI base64 (натив base64-decode). JSON — натив json-parse.
;;
;; Поддержанное ПОДМНОЖЕСТВО glTF 2.0 (честно задокументировано в
;; LIMITATIONS.md):
;;   — сцены/узлы: обход сцены по умолчанию; transform узла («matrix»
;;     column-major ИЛИ TRS с кватернионом) запекается в вершины;
;;   — примитивы: только mode 4 (TRIANGLES);
;;   — атрибуты: POSITION (обязателен), NORMAL и TEXCOORD_0 (опциональны;
;;     нормали читаются в меш, но шейдинг движка flat — per-vertex N
;;     конвейер не использует);
;;   — accessor: FLOAT (5126), UNSIGNED_SHORT (5123), UNSIGNED_INT (5125),
;;     UNSIGNED_BYTE (5121); bufferView c byteStride;
;;   — буферы: uri = внешний .bin или data:application/octet-stream;base64,…
;;   — текстуры: только PNG (свой декодер png.rs): файл или data-URI;
;;     из материалов — baseColorFactor (тинт) и baseColorTexture;
;;   — оси: glTF y-up совпадает с движком; UV origin сверху — совпадает.
;;   — НЕТ: .glb-контейнер, skinning/анимации, PBR-текстуры, камеры glTF.
;;
;; IEEE-754 binary32 декодируется на Lisp (у движка нет bitcast):
;; знак/экспонента/мантисса через mod/floor, 2^k — из таблицы.
;;
;; API:
;;   (gltf-load путь) → список мешей-таблиц: 'pos/'uv/'nrm/'idx (F32Vec),
;;                      'tex (id текстуры или 0), 'r/'g/'b (baseColorFactor),
;;                      'verts/'tris. Кэш по пути — повторный вызов мгновенен.
;;   (draw-gltf ms model mvp tint [материал]) — отрисовать список мешей.
;;   (gltf-cache-clear!) — сброс кэша (hot reload).

(define *gltf-cache* (make-table))

;; ——— биты → числа ———

;; таблица 2^k для k=-149..127 (индекс k+149): точные степени двойки в f32
(define *gltf-pow2* #f)
(define (gltf-pow2 k)
  (if *gltf-pow2*
      #f
      (begin
        (set! *gltf-pow2* (make-f32vec 277 0.0))
        (f32set! *gltf-pow2* 149 1.0)
        (let ((i 148) (x 0.5))
          (while (>= i 0)
            (f32set! *gltf-pow2* i x)
            (set! i (- i 1))
            (set! x (/ x 2))))
        (let ((i 150) (x 2.0))
          (while (< i 277)
            (f32set! *gltf-pow2* i x)
            (set! i (+ i 1))
            (set! x (* x 2))))))
  (f32ref *gltf-pow2* (+ k 149)))

(define (gltf-u16 b o) (+ (f32ref b o) (* (f32ref b (+ o 1)) 256)))

(define (gltf-u32 b o)
  (+ (f32ref b o)
     (* 256 (f32ref b (+ o 1)))
     (* 65536 (f32ref b (+ o 2)))
     (* 16777216 (f32ref b (+ o 3)))))

;; IEEE-754 binary32 из 4 байт LE: u = знак | экспонента(8) | мантисса(23);
;; e=0 — денормал (m·2^-149), e=255 — ошибка (в accessor их не бывает)
(define (gltf-f32 b o)
  (let ((u (gltf-u32 b o)))
    (if (= u 0)
        0.0
        (let* ((eu (mod u 2147483648))
               (e (floor (/ eu 8388608)))
               (m (mod eu 8388608)))
          (if (= e 255)
              (error "gltf-f32: inf/nan в accessor")
              (* (if (= eu u) 1.0 -1.0)
                 (if (= e 0)
                     (* m (gltf-pow2 -149))
                     (* (+ 1.0 (* m 0.00000011920928955078125))
                        (gltf-pow2 (- e 127))))))))))

;; размер компонента и чтение компонента по абсолютному смещению
(define (gltf-esize ct)
  (cond ((= ct 5126) 4)
        ((= ct 5125) 4)
        ((= ct 5123) 2)
        ((= ct 5121) 1)
        (else (error "gltf: componentType не поддержан" ct))))

(define (gltf-comp b ct o)
  (cond ((= ct 5126) (gltf-f32 b o))
        ((= ct 5125) (gltf-u32 b o))
        ((= ct 5123) (gltf-u16 b o))
        ((= ct 5121) (f32ref b o))
        (else (error "gltf: componentType не поддержан" ct))))

(define (gltf-comp-count type)
  (cond ((string=? type "SCALAR") 1)
        ((string=? type "VEC2") 2)
        ((string=? type "VEC3") 3)
        ((string=? type "VEC4") 4)
        (else (error "gltf: тип accessor не поддержан" type))))

;; ——— буферы и accessors ———

;; буфер glTF → f32-вектор байтов: data-URI (base64) или внешний .bin
(define (gltf-buffer-bytes base buf)
  (let ((uri (table-get buf "uri" #f)))
    (cond ((not uri)
           (error "gltf: buffer без uri (.glb-контейнер не поддерживается)"))
          ((string=? (gltf-prefix uri 5) "data:") (base64-decode uri))
          (else (file-read-bytes (string-append base uri))))))

;; accessor → F32Vec плоских компонентов (count × comps)
(define (gltf-acc doc bufs acc)
  (let* ((n (table-get acc "count" 0))
         (nc (gltf-comp-count (table-get acc "type" "SCALAR")))
         (ct (table-get acc "componentType" 5126))
         (es (gltf-esize ct))
         (bv (nth (table-get doc "bufferViews" '())
                  (table-get acc "bufferView" 0)))
         (b (nth bufs (table-get bv "buffer" 0)))
         ;; byteStride из bufferView; если не задан — плотная упаковка
         (stride (max (* es nc) (table-get bv "byteStride" 0)))
         (off (+ (table-get bv "byteOffset" 0) (table-get acc "byteOffset" 0)))
         (out (make-f32vec (* n nc) 0.0)))
    (dotimes (i n)
      (let ((base (+ off (* i stride))))
        (dotimes (c nc)
          (f32set! out (+ (* i nc) c)
                   (gltf-comp b ct (+ base (* c es)))))))
    out))

;; ——— строки/пути (dialect без substring — собираем из string-at) ———

(define (gltf-prefix s n)
  (define (go i acc)
    (if (>= i n)
        acc
        (go (+ i 1) (string-append acc (string-at s i)))))
  (go 0 ""))

;; каталог gltf-файла: «assets/models/x/Lantern.gltf» → «assets/models/x/»
;; (URI картинок/буферов в glTF отсчитываются от файла сцены)
(define (gltf-dir path)
  (define (last-slash i best)
    (if (>= i (string-length path))
        best
        (last-slash (+ i 1)
                    (if (string=? (string-at path i) "/") i best))))
  (define (take i acc)
    (if (> i (last-slash 0 -1))
        acc
        (take (+ i 1) (string-append acc (string-at path i)))))
  (take 0 ""))

;; ——— текстуры (PNG: файл или data-URI) ———

(define (gltf-texture doc base tex-idx)
  (cond ((or (not tex-idx) (< tex-idx 0)) 0)
        (else
         (let* ((tex (nth (table-get doc "textures" '()) tex-idx))
                (img (nth (table-get doc "images" '())
                          (table-get tex "source" 0)))
                (uri (table-get img "uri" #f)))
           (cond ((not uri) 0) ; изображения из bufferView не поддержаны
                 ((string=? (gltf-prefix uri 5) "data:")
                  (texture-load-png (base64-decode uri)))
                 (else (texture-load (string-append base uri))))))))

;; ——— матрицы узлов ———

;; glTF matrix (column-major) → row-major движка (трансляция в 3/7/11)
(define (gltf-mat-from-json m)
  (f32-from-list
   (list (nth m 0) (nth m 4) (nth m 8) (nth m 12)
         (nth m 1) (nth m 5) (nth m 9) (nth m 13)
         (nth m 2) (nth m 6) (nth m 10) (nth m 14)
         (nth m 3) (nth m 7) (nth m 11) (nth m 15))))

;; TRS узла (кватернион x y z w) → M = T·R·S (column-вектор), row-major
(define (gltf-trs t r s)
  (let ((x (nth r 0)) (y (nth r 1)) (z (nth r 2)) (w (nth r 3)))
    (let ((xx (* x x)) (yy (* y y)) (zz (* z z)))
      (f32-from-list
       (list (* (- 1.0 (* 2.0 (+ yy zz))) (nth s 0))
             (* (- (* 2.0 x y) (* 2.0 z w)) (nth s 1))
             (* (+ (* 2.0 x z) (* 2.0 y w)) (nth s 2))
             (nth t 0)
             (* (+ (* 2.0 x y) (* 2.0 z w)) (nth s 0))
             (* (- 1.0 (* 2.0 (+ xx zz))) (nth s 1))
             (* (- (* 2.0 y z) (* 2.0 x w)) (nth s 2))
             (nth t 1)
             (* (- (* 2.0 x z) (* 2.0 y w)) (nth s 0))
             (* (+ (* 2.0 y z) (* 2.0 x w)) (nth s 1))
             (* (- 1.0 (* 2.0 (+ xx yy))) (nth s 2))
             (nth t 2)
             0 0 0 1)))))

;; матрица мира узла: «matrix» ИЛИ TRS (дефолты glTF: identity)
(define (gltf-node-mat node)
  (cond ((table-has? node "matrix")
         (gltf-mat-from-json (table-get node "matrix")))
        (else
         (gltf-trs (table-get node "translation" (list 0 0 0))
                   (table-get node "rotation" (list 0 0 0 1))
                   (table-get node "scale" (list 1 1 1))))))

;; ——— обход сцены: → список (mesh-idx матрица-мира) ———

(define (gltf-node doc nodes node-idx parent walk)
  (let* ((node (nth nodes node-idx))
         (world (if parent
                   (mat4-mul parent (gltf-node-mat node))
                   (gltf-node-mat node))))
    (define (go kids)
      (if (null? kids)
          walk
          (go (cdr kids)
              (gltf-node doc nodes (car kids) world walk))))
    (if (table-has? node "mesh")
        (go (table-get node "children" '())
            (cons (list (table-get node "mesh") world) walk))
        (go (table-get node "children" '()) walk))))

(define (gltf-scene-walk doc)
  (let* ((nodes (table-get doc "nodes" '()))
         (scenes (table-get doc "scenes" '()))
         (scene (nth scenes (table-get doc "scene" 0))))
    (define (go kids acc)
      (if (null? kids)
          acc
          (go (cdr kids)
              (gltf-node doc nodes (car kids) #f acc))))
    (go (table-get scene "nodes" '()) '())))

;; ——— примитив → меш-таблица (вершины запечены матрицей мира узла) ———

(define (gltf-prim doc bufs base prim mat4)
  (let* ((attrs (table-get prim "attributes"))
         (accs (table-get doc "accessors" '()))
         (pos-i (table-get attrs "POSITION" -1))
         (mode (table-get prim "mode" 4)))
    (if (< pos-i 0)
        (error "gltf: у примитива нет POSITION")
        #f)
    (if (not (= mode 4))
        (error "gltf: поддержан только mode 4 (TRIANGLES), а не" mode)
        #f)
    (let* ((pos-a (nth accs pos-i))
           (n (table-get pos-a "count" 0))
           (pos (gltf-acc doc bufs pos-a))
           (uv (if (table-has? attrs "TEXCOORD_0")
                   (gltf-acc doc bufs (nth accs (table-get attrs "TEXCOORD_0")))
                   #f))
           (nrm (if (table-has? attrs "NORMAL")
                    (gltf-acc doc bufs (nth accs (table-get attrs "NORMAL")))
                    #f))
           (idx (gltf-acc doc bufs
                          (nth accs (table-get prim "indices" 0))))
           ;; мировые координаты: x' = m0·x + m1·y + m2·z + m3 (affine)
           (pos2 (make-f32vec (* n 3) 0.0)))
      (dotimes (i n)
        (let* ((j (* i 3))
               (x (f32ref pos j))
               (y (f32ref pos (+ j 1)))
               (z (f32ref pos (+ j 2))))
          (f32set! pos2 j
                   (+ (* (f32ref mat4 0) x) (* (f32ref mat4 1) y)
                      (* (f32ref mat4 2) z) (f32ref mat4 3)))
          (f32set! pos2 (+ j 1)
                   (+ (* (f32ref mat4 4) x) (* (f32ref mat4 5) y)
                      (* (f32ref mat4 6) z) (f32ref mat4 7)))
          (f32set! pos2 (+ j 2)
                   (+ (* (f32ref mat4 8) x) (* (f32ref mat4 9) y)
                      (* (f32ref mat4 10) z) (f32ref mat4 11)))))
      ;; материал: baseColorFactor → тинт, baseColorTexture → текстура
      (let* ((mat (if (table-has? prim "material")
                      (nth (table-get doc "materials" '())
                           (table-get prim "material"))
                      #f))
             (bcf (if mat
                      (table-get mat "baseColorFactor"
                                 (list 1.0 1.0 1.0 1.0))
                      (list 1.0 1.0 1.0 1.0)))
             (tex-i (if mat
                        (table-get (table-get mat "baseColorTexture"
                                              (make-table))
                                   "source" -1)
                        -1))
             (m (make-table)))
        (table-set! m 'pos pos2)
        (table-set! m 'uv uv)
        (table-set! m 'nrm nrm)
        (table-set! m 'idx idx)
        (table-set! m 'tex (gltf-texture doc base tex-i))
        (table-set! m 'r (car bcf))
        (table-set! m 'g (cadr bcf))
        (table-set! m 'b (caddr bcf))
        (table-set! m 'verts n)
        (table-set! m 'tris (/ (f32vec-length idx) 3))
        m))))

(define (gltf-mesh doc bufs base mesh mat4)
  (define (go prims acc)
    (if (null? prims)
        acc
        (go (cdr prims)
            (append acc
                    (list (gltf-prim doc bufs base (car prims) mat4))))))
  (go (table-get mesh "primitives" '()) '()))

;; ——— загрузка с кэшем по пути ———

(define (gltf-load path)
  (if (table-has? *gltf-cache* path)
      (table-get *gltf-cache* path)
      (let* ((doc (json-parse (file-read path)))
             (ver (table-get (table-get doc "asset" (make-table))
                             "version" ""))
             (base (gltf-dir path))
             (bufs (map (lambda (b) (gltf-buffer-bytes base b))
                        (table-get doc "buffers" '())))
             (mesh-lists '()))
        (if (= (string-length ver) 0)
            (error "gltf: нет asset.version — это не glTF")
            #f)
        (if (string=? (gltf-prefix ver 1) "2")
            #f
            (error "gltf: ожидался glTF 2.0, а не" ver))
        (define (go pairs)
          (if (null? pairs)
              #f
              (begin
                (set! mesh-lists
                      (append mesh-lists
                              (gltf-mesh doc bufs base
                                         (nth (table-get doc "meshes" '())
                                              (car (car pairs)))
                                         (cadr (car pairs)))))
                (go (cdr pairs)))))
        (go (gltf-scene-walk doc))
        (table-set! *gltf-cache* path mesh-lists)
        mesh-lists)))

(define (gltf-cache-clear!) (table-clear! *gltf-cache*) #t)

;; ——— отрисовка ———
;; (draw-gltf ms model mvp tint [материал]) — список мешей из gltf-load;
;; тинт умножается на baseColorFactor примитива; меш без текстуры — цветом.
(define (draw-gltf ms model mvp tint . opt)
  (define (go lst)
    (if (null? lst)
        #f
        (let ((m (car lst)))
          (if (null? opt)
              #f
              (use-material! (car opt)))
          (let ((r (* (car tint) (table-get m 'r 1.0)))
                (g (* (cadr tint) (table-get m 'g 1.0)))
                (b (* (caddr tint) (table-get m 'b 1.0)))
                (tex (table-get m 'tex 0))
                (uv (table-get m 'uv #f)))
            (if (and (> tex 0) uv)
                (gpu3d-mesh-tex (table-get m 'pos) uv
                                (table-get m 'idx) tex model mvp r g b)
                (gpu3d-mesh (table-get m 'pos) (table-get m 'idx)
                            model mvp r g b))
            (go (cdr lst))))))
  (go ms))
