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
(define *pose-cache-key* "")
(define *pose-cache-val* #f)

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
        ((string=? type "MAT4") 16)
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

;; PBR-блок материала: baseColorTexture/baseColorFactor живут внутри
;; pbrMetallicRoughness (спецификация glTF 2.0); верхний уровень — фолбэк
(define (gltf-pbr mat)
  (if (table-has? mat "pbrMetallicRoughness")
      (table-get mat "pbrMetallicRoughness")
      mat))

;; ——— текстуры (PNG: файл или data-URI) ———

(define (gltf-texture doc base tex-idx)
  ;; texture info: {index: N} (спецификация glTF 2.0; "source" — фолбэк)
  (cond ((or (not tex-idx) (< tex-idx 0)) 0)
        (else
         (let* ((tex (nth (table-get doc "textures" '()) tex-idx))
                (img (nth (table-get doc "images" '())
                          (if (table-has? tex "index")
                              (table-get tex "index" 0)
                              (table-get tex "source" 0))))
                (uri (table-get img "uri" #f)))
           (cond ((not uri) 0) ; изображения из bufferView не поддержаны
                 ((string=? (gltf-prefix uri 5) "data:")
                  (texture-load-png (base64-decode uri)))
                 (else
                 ;; Модель без текстуры — это модель без текстуры, а не
                 ;; упавшая игра: glTF-конвертеры регулярно теряют часть
                 ;; изображений (и на Linux ещё и регистр букв в именах).
                 ;; Отсутствующий файл даёт 0 («нет текстуры»), как и
                 ;; изображение из bufferView.
                 (let ((path (string-append base uri)))
                   (if (file-exists? path)
                       (texture-load path)
                       0))))))))

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
    (define (go kids acc)
      (if (null? kids)
          acc
          (go (cdr kids)
              (gltf-node doc nodes (car kids) world acc))))
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
           (pos2 (make-f32vec (* n 3) 0.0))
           (nrm2 (if nrm (make-f32vec (* n 3) 0.0) #f)))
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
      (if nrm
          (dotimes (i n)
            (let* ((j (* i 3))
                   (nx (f32ref nrm j))
                   (ny (f32ref nrm (+ j 1)))
                   (nz (f32ref nrm (+ j 2)))
                   (tx (+ (* (f32ref mat4 0) nx) (* (f32ref mat4 1) ny) (* (f32ref mat4 2) nz)))
                   (ty (+ (* (f32ref mat4 4) nx) (* (f32ref mat4 5) ny) (* (f32ref mat4 6) nz)))
                   (tz (+ (* (f32ref mat4 8) nx) (* (f32ref mat4 9) ny) (* (f32ref mat4 10) nz)))
                   (l (sqrt (+ (* tx tx) (* ty ty) (* tz tz))))
                   (inv-l (if (> l 1e-9) (/ 1.0 l) 1.0)))
              (f32set! nrm2 j (* tx inv-l))
              (f32set! nrm2 (+ j 1) (* ty inv-l))
              (f32set! nrm2 (+ j 2) (* tz inv-l))))
          #f)
      ;; материал: baseColorFactor → тинт, baseColorTexture → текстура
      (let* ((mat (if (table-has? prim "material")
                      (nth (table-get doc "materials" '())
                           (table-get prim "material"))
                      #f))
             (pbr (if mat (gltf-pbr mat) #f))
             (bcf (if pbr
                      (table-get pbr "baseColorFactor"
                                 (list 1.0 1.0 1.0 1.0))
                      (list 1.0 1.0 1.0 1.0)))
             (tex-i (if pbr
                        (table-get (table-get pbr "baseColorTexture"
                                              (make-table))
                                   "index" -1)
                        -1))
             (m (make-table)))
        (table-set! m 'pos pos2)
        (table-set! m 'uv uv)
        (table-set! m 'nrm (or nrm2 #f))
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
                (uv (table-get m 'uv #f))
                (nrm (table-get m 'nrm #f)))
            (if (and (> tex 0) uv)
                (if nrm
                    (gpu3d-mesh-tex (table-get m 'pos) uv
                                    (table-get m 'idx) tex model mvp r g b nrm)
                    (gpu3d-mesh-tex (table-get m 'pos) uv
                                    (table-get m 'idx) tex model mvp r g b))
                (if nrm
                    (gpu3d-mesh (table-get m 'pos) (table-get m 'idx)
                                model mvp r g b nrm)
                    (gpu3d-mesh (table-get m 'pos) (table-get m 'idx)
                                model mvp r g b)))
            (go (cdr lst))))))
  (go ms))


;; ——— скиннинг и анимации (этап 21) ———
;; Подмножество: skin.joints + inverseBindMatrices (MAT4), JOINTS_0
;; (UNSIGNED_SHORT/UNSIGNED_BYTE VEC4), WEIGHTS_0 (FLOAT VEC4),
;; анимации — каналы rotation/translation/scale с ЛИНЕЙНОЙ интерполяцией
;; (CUBICSPLINE/STEP не поддержаны), кватернионы — nlerp с коррекцией знака.
;; Иерархия: мир узлов считается РЕКУРСИВНО от корней сцены (порядок узлов
;; в файле не важен), кэша поз нет — сцены маленькие.

;; примитив для скина: вершины в bind-пространстве (запекания нет),
;; JOINTS_0/WEIGHTS_0, индексы — при отсутствии генерируются последовательно
(define (gltf-prim-skin doc bufs prim)
  (let* ((attrs (table-get prim "attributes"))
         (accs (table-get doc "accessors" '()))
         (pos-i (table-get attrs "POSITION" -1)))
    (if (< pos-i 0)
        (error "gltf-skin: нет POSITION")
        #f)
    (let* ((pos-a (nth accs pos-i))
           (n (table-get pos-a "count" 0))
           (pos (gltf-acc doc bufs pos-a))
           (uv (if (table-has? attrs "TEXCOORD_0")
                   (gltf-acc doc bufs (nth accs (table-get attrs "TEXCOORD_0")))
                   #f))
           (joints (gltf-acc doc bufs (nth accs (table-get attrs "JOINTS_0"))))
           (weights (gltf-acc doc bufs (nth accs (table-get attrs "WEIGHTS_0"))))
           (m (make-table)))
      (table-set! m 'pos pos)
      (table-set! m 'uv uv)
      (table-set! m 'joints joints)
      (table-set! m 'weights weights)
      (table-set! m 'verts n)
      ;; неиндексированный примитив (Fox): трисы идут потоком вершин
      (table-set! m 'tris (/ n 3))
      m)))

;; inverseBindMatrices accessor (column-major) → F32Vec 16×J row-major
(define (gltf-ibm doc bufs skin)
  (let ((a (nth (table-get doc "accessors" '())
                (table-get skin "inverseBindMatrices" 0)))
        (J (length (table-get skin "joints" '()))))
    (let ((col (gltf-acc doc bufs a))
          (out (make-f32vec (* 16 J) 0.0)))
      (dotimes (j J)
        (dotimes (r 16)
          ;; col-major элемент (r mod 4, r div 4) → row-major слот r
          (f32set! out (+ (* j 16) r)
                   (f32ref col (+ (* j 16)
                                  (+ (* (mod r 4) 4) (floor (/ r 4))))))))
      out)))

;; x входит в список? (в диалекте нет member)
(define (gltf-in-list x lst)
  (cond ((null? lst) #f)
        ((= x (car lst)) #t)
        (else (gltf-in-list x (cdr lst)))))

;; иерархия узлов: родители (f32vec, -1 = корень) + локальные матрицы
;; + базовые TRS-компоненты (анимация замещает только свои каналы)
(define (gltf-hierarchy doc)
  (let* ((nodes (table-get doc "nodes" '()))
         (n (length nodes)))
    (let ((parents (make-f32vec n -1.0))
          (locals '())
          (bts '()) (brs '()) (bss '()))
      (define (go i)
        (if (>= i n)
            #f
            (let ((node (nth nodes i)))
              (set! locals (append locals (list (gltf-node-mat node))))
              (set! bts (append bts (list (table-get node "translation" (list 0 0 0)))))
              (set! brs (append brs (list (table-get node "rotation" (list 0 0 0 1)))))
              (set! bss (append bss (list (table-get node "scale" (list 1 1 1)))))
              (go (+ i 1)))))
      (go 0)
      ;; родители — второй проход (порядок узлов не важен)
      (define (go2 i)
        (if (>= i n)
            #f
            (let ((node (nth nodes i)))
              (define (find-parent k)
                (cond ((>= k n) -1.0)
                      ((gltf-in-list i (table-get (nth nodes k) "children" '()))
                       (* k 1.0))
                      (else (find-parent (+ k 1)))))
              (f32set! parents i (find-parent 0))
              (go2 (+ i 1)))))
      (go2 0)
      (list parents locals bts brs bss))))

;; (gltf-load-skin путь) — скин-модель + анимации, кэш по пути
(define (gltf-load-skin path)
  (if (table-has? *gltf-cache* path)
      (table-get *gltf-cache* path)
      (let* ((doc (json-parse (file-read path)))
             (ver (table-get (table-get doc "asset" (make-table)) "version" ""))
             (base (gltf-dir path))
             (bufs (map (lambda (b) (gltf-buffer-bytes base b))
                        (table-get doc "buffers" '()))))
        (if (string=? (gltf-prefix ver 1) "2")
            #f
            (error "gltf-skin: ожидался glTF 2.0"))
        ;; первый узел сцены с мешем и скином
        (let* ((nodes (table-get doc "nodes" '()))
               (scene (nth (table-get doc "scenes" '()) (table-get doc "scene" 0)))
               (mesh-node (gltf-find-skin-node nodes (table-get scene "nodes" '())))
               (node (nth nodes mesh-node))
               (mesh (nth (table-get doc "meshes" '()) (table-get node "mesh")))
               (prim (car (table-get mesh "primitives" '())))
               (skin (nth (table-get doc "skins" '()) (table-get node "skin")))
               (jnodes (table-get skin "joints" '()))
               (mat (if (table-has? prim "material")
                        (nth (table-get doc "materials" '()) (table-get prim "material"))
                        #f))
               (pbr (if mat (gltf-pbr mat) #f))
               (tex-i (if pbr
                          (table-get (table-get pbr "baseColorTexture" (make-table)) "index" -1)
                          -1))
               (pr (gltf-prim-skin doc bufs prim))
               (hi (gltf-hierarchy doc))
               (m (make-table)))
          (table-set! m 'pos (table-get pr 'pos))
          (table-set! m 'uv (table-get pr 'uv))
          (table-set! m 'joints (table-get pr 'joints))
          (table-set! m 'weights (table-get pr 'weights))
          (table-set! m 'verts (table-get pr 'verts))
          (table-set! m 'tris (table-get pr 'tris))
          (table-set! m 'tex (gltf-texture doc base tex-i))
          (table-set! m 'jnodes jnodes)
          (table-set! m 'ibm (gltf-ibm doc bufs skin))
          (table-set! m 'node-parent (car hi))
          (table-set! m 'node-local (cadr hi))
          (table-set! m 'node-t (nth hi 2))
          (table-set! m 'node-r (nth hi 3))
          (table-set! m 'node-s (nth hi 4))
          (table-set! m 'mesh-node mesh-node)
          (table-set! m 'anims (gltf-anims doc bufs))
          (table-set! *gltf-cache* path m)
          m))))

(define (gltf-find-skin-node nodes roots)
  (define (go idxs)
    (if (null? idxs)
        (error "gltf-skin: узел с мешем+скином не найден")
        (let ((node (nth nodes (car idxs))))
          (if (and (table-has? node "mesh") (table-has? node "skin"))
              (car idxs)
              (go (cdr idxs))))))
  (go roots))

;; ——— анимации: каналы (node path times values count), линейные ———

(define (gltf-anim-chan doc bufs anim)
  (define (go samplers chans acc)
    (if (null? chans)
        acc
        (let* ((ch (car chans))
               (samp (nth (table-get anim "samplers" '())
                          (table-get ch "sampler" 0)))
               (target (table-get ch "target" (make-table)))
               (times (gltf-acc doc bufs
                                (nth (table-get doc "accessors" '())
                                     (table-get samp "input" 0))))
               (out-a (nth (table-get doc "accessors" '())
                           (table-get samp "output" 0)))
               (nc (gltf-comp-count (table-get out-a "type" "SCALAR")))
               (vals (gltf-acc doc bufs out-a)))
          (go samplers
              (cdr chans)
              (append acc
                      (list (list (table-get target "node" -1)
                                  (table-get target "path" "")
                                  times vals
                                  (table-get out-a "count" 0) nc)))))))
  (go (table-get anim "samplers" '()) (table-get anim "channels" '()) '()))

(define (gltf-anims doc bufs)
  (define (go anims acc)
    (if (null? anims)
        acc
        (let* ((a (car anims))
               (chans (gltf-anim-chan doc bufs a))
               ;; длительность: max последнего времени
               (dur 0.0))
          (define (find-dur chans)
            (if (null? chans)
                #f
                (let* ((c (car chans))
                       (times (nth c 2))
                       (n (nth c 4)))
                  (if (> n 0)
                      (set! dur (max dur (f32ref times (- n 1))))
                      #f)
                  (find-dur (cdr chans)))))
          (find-dur chans)
          (go (cdr anims)
              (append acc (list (list (table-get a "name" "") dur chans)))))))
  (go (table-get doc "animations" '()) '()))

;; остаток для дробного времени (mod движка целочисленный)
(define (gltf-frac time dur)
  (- time (* dur (floor (/ time dur)))))

;; линейная выборка канала в момент t → список nc значений
(define (gltf-sample times vals count nc t)
  (define (go i)
    (if (or (>= i (- count 1)) (<= (f32ref times (+ i 1)) t))
        (min i (- count 2))
        (go (+ i 1))))
  (if (<= count 1)
      (let ((acc '()))
        (dotimes (c nc) (set! acc (append acc (list (f32ref vals c)))))
        acc)
      (let* ((i (go 0))
             (t0 (f32ref times i))
             (t1 (f32ref times (+ i 1)))
             (f (if (> (- t1 t0) 1e-6) (/ (- t t0) (- t1 t0)) 0.0))
             (acc '()))
        (dotimes (c nc)
          (let ((v0 (f32ref vals (+ (* i nc) c)))
                (v1 (f32ref vals (+ (* (+ i 1) nc) c))))
            (set! acc (append acc (list (+ (* v0 (- 1.0 f)) (* v1 f)))))))
        acc)))

;; нормировка кватерниона (lerp двух единичных не единичен —
;; без этого конечности сжимаются/растягиваются)
(define (gltf-quat-norm q)
  (let ((x (car q)) (y (cadr q)) (z (caddr q)) (w (cadddr q)))
    (let ((l (max 1e-9 (sqrt (+ (* x x) (* y y) (* z z) (* w w))))))
      (list (/ x l) (/ y l) (/ z l) (/ w l)))))

;; nlerp кватернионов с коррекцией знака (для канала rotation)
(define (gltf-quat-nlerp q0 q1 f)
  (let ((q1s (if (< (+ (* (car q0) (car q1))
                       (* (cadr q0) (cadr q1))
                       (* (caddr q0) (caddr q1))
                       (* (cadddr q0) (cadddr q1)))
                    0.0)
                 (list (- (car q1)) (- (cadr q1)) (- (caddr q1)) (- (cadddr q1)))
                 q1)))
    (let ((x (+ (* (car q0) (- 1.0 f)) (* (car q1s) f)))
          (y (+ (* (cadr q0) (- 1.0 f)) (* (cadr q1s) f)))
          (z (+ (* (caddr q0) (- 1.0 f)) (* (caddr q1s) f)))
          (w (+ (* (cadddr q0) (- 1.0 f)) (* (cadddr q1s) f))))
      (let ((l (max 1e-9 (sqrt (+ (* x x) (* y y) (* z z) (* w w))))))
        (list (/ x l) (/ y l) (/ z l) (/ w l))))))

;; поза в момент time → (jmat mesh-world):
;; локальные матрицы из иерархии, каналы анимации замещают TRS-компоненту,
;; мир — рекурсивно от корней, сустав = мир × inverseBind
(define (gltf-pose-skin model anim-name time)
  ;; кэш на кадр: сцена синхронизирует фазу анимации и квантует время —
  ;; все инстансы модели за кадр получают одну позу (5 лис = 1 расчёт)
  (let ((ck (string-append anim-name ":"
                           (number->string (floor (* time 8.0))))))
    (if (string=? ck *pose-cache-key*)
        *pose-cache-val*
        (begin
          (set! *pose-cache-key* ck)
          (set! *pose-cache-val* (gltf-pose-skin-uncached model anim-name time))
          *pose-cache-val*))))

(define (gltf-pose-skin-uncached model anim-name time)
  ;; 1) замещения каналов анимации; 2) локальные матрицы (dotimes);
  ;; 3) мир узла — цепочкой родителей снизу вверх (без рекурсии);
  ;; 4) сустав = мир × inverseBind. Возврат: (jmat mesh-world)
  (let* ((parents (table-get model 'node-parent))
         (locals0 (table-get model 'node-local))
         (nodes-n (length parents))
         (anims (table-get model 'anims '()))
         (jnodes (table-get model 'jnodes))
         (J (length jnodes))
         (ibm (table-get model 'ibm))
         (over (make-table))
         (dur 1.0)
         (anim #f))
    (define (find-anim as)
      (cond ((null? as) #f)
            ((string=? (car (car as)) anim-name) (car as))
            (else (find-anim (cdr as)))))
    (set! anim (find-anim anims))
    (if anim
        (begin
          (set! dur (max 1e-3 (cadr anim)))
          (define (apply-chans chans)
            (if (null? chans)
                #f
                (let* ((c (car chans))
                       (node (car c))
                       (pth (cadr c))
                       (times (nth c 2))
                       (vals (nth c 3))
                       (count (nth c 4))
                       (nc (nth c 5))
                       (key (string-append (number->string node) ":" pth)))
                  (table-set! over key
                              (gltf-sample times vals count nc
                                           (gltf-frac time dur)))
                  (apply-chans (cdr chans)))))
          (apply-chans (caddr anim)))
        #f)
    ;; локальные матрицы с анимацией: канал замещает ТОЛЬКО свой компонент,
    ;; базовые T/R/S узла сохраняются (иначе скелет схлопывается)
    (let ((locals '())
          (bts (table-get model 'node-t))
          (brs (table-get model 'node-r))
          (bss (table-get model 'node-s)))
      (dotimes (i nodes-n)
        (let* ((key-r (string-append (number->string i) ":rotation"))
               (key-t (string-append (number->string i) ":translation"))
               (key-s (string-append (number->string i) ":scale"))
               (t (if (table-has? over key-t)
                      (table-get over key-t)
                      (nth bts i)))
               (r (if (table-has? over key-r)
                      (gltf-quat-norm (table-get over key-r))
                      (nth brs i)))
               (sc (if (table-has? over key-s)
                       (table-get over key-s)
                       (nth bss i))))
          (set! locals (append locals (list (gltf-trs t r sc))))))
      ;; мир узла: цепочка родителей, перемножение от корня вниз
      (let ((worlds (make-f32vec (* 16 nodes-n) 0.0)))
        (define (rows-of i)
          ;; восстановить посчитанную матрицу как F32Vec
          (let ((acc '()))
            (dotimes (k 16)
              (set! acc (append acc (list (f32ref worlds (+ (* i 16) k))))))
            (f32-from-list acc)))
        (define (world-of i)
          (if (not (= (f32ref worlds (* i 16)) 0.0))
              (rows-of i)
              (let ((chain '()))
                ;; вверх: (cons j acc) → (root ... i)
                (define (up j acc)
                  (let ((p (floor (f32ref parents j))))
                    (if (< p 0)
                        (cons j acc)
                        (up p (cons j acc)))))
                (set! chain (up i (list)))
                ;; от корня вниз: m = I · local[root] · local[..] · local[i]
                (let ((m (mat-identity)))
                  (define (fold cs)
                    (if (null? cs)
                        #f
                        (begin
                          (set! m (mat4-mul m (nth locals (car cs))))
                          (fold (cdr cs)))))
                  (fold chain)
                  (dotimes (k 16)
                    (f32set! worlds (+ (* i 16) k) (f32ref m k)))
                  m))))
        ;; все узлы + суставы
        (dotimes (i nodes-n) (world-of i))
        (let ((jmat (make-f32vec (* 16 J) 0.0)))
          (dotimes (j J)
            (let ((w (world-of (nth jnodes j))))
              (dotimes (r 4)
                (dotimes (c 4)
                  (let ((acc 0.0))
                    (dotimes (k 4)
                      (set! acc
                            (+ acc (* (f32ref w (+ (* r 4) k))
                                      (f32ref ibm (+ (* j 16) (+ (* k 4) c)))))))
                    (f32set! jmat (+ (* j 16) (+ (* r 4) c)) acc))))))
          ;; мир узла с мешем (у скиннед-моделей по спецификации glTF он
          ;; игнорируется — используем только для MODEL-матрицы сцены)
          (let ((mw (world-of (table-get model 'mesh-node)))
                (mwv (make-f32vec 16 0.0)))
            (dotimes (k 16)
              (f32set! mwv k (f32ref mw k)))
            (list jmat mwv)))))))
;; отрисовка скин-модели в момент time:
;; (draw-gltf-skin model anim time model-mat mvp tint-r tint-g tint-b)
(define (draw-gltf-skin model anim time model-mat mvp tr tg tb)
  (let* ((pose (gltf-pose-skin model anim time))
         (jmat (car pose))
         (mworld (cadr pose))
         (full (mat4-mul model-mat mworld)))
    (gpu3d-mesh-skin (table-get model 'pos)
                     (table-get model 'joints)
                     (table-get model 'weights)
                     jmat
                     full mvp
                     (table-get model 'uv)
                     (table-get model 'tex 0)
                     tr tg tb)))
