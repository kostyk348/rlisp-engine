// src/cli.rs — Разбор аргументов командной строки и справка

#[derive(Debug, Clone)]
pub struct Args {
    pub script: String,
    pub ascii: bool,
    pub capture: Option<String>,
    pub every: usize,
    pub frames: u64,
    pub fps: f64,
    pub width: usize,
    pub height: usize,
    pub input: Option<String>,
    pub stdin_keys: bool,
    pub fast: bool,
    pub bench: bool,
    pub bench_parse: bool,
    pub bench_parse_allocs: bool,
    pub eval_code: Option<String>,
    pub serve: bool,
    pub port: u16,
    pub mcp: bool,
    pub wgpu: bool,
    /// --record <файл>: писать детерминированный прогон (вход + seed + hash кадров).
    pub record: Option<String>,
    /// --replay <файл>: воспроизвести прогон и сверить цепочку хешей.
    pub replay: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            script: "assets/lisp/main.lisp".into(),
            ascii: false,
            capture: None,
            every: 3,
            frames: u64::MAX,
            fps: 30.0,
            width: 320,
            height: 240,
            input: None,
            stdin_keys: false,
            fast: false,
            bench: false,
            bench_parse: false,
            bench_parse_allocs: false,
            eval_code: None,
            serve: false,
            port: 7300,
            mcp: false,
            wgpu: false,
            record: None,
            replay: None,
        }
    }
}

pub const USAGE: &str = "\
rlisp-engine — игровой движок как Lisp-машина (Rust = только HAL)

ЗАПУСК ДЕМО «ВЫЖИВАНИЕ» (весь игровой код — Lisp):
  cargo run --release
  cargo run --release -- --ascii --stdin-keys   # играть в терминале: wasd, q — выход

ЗАПУСК 3D-ДЕМО «ORBITAL» (3D-пайплайн на Lisp над HAL-растром):
  cargo run --release -- --script assets/lisp/main3d.lisp --ascii --stdin-keys
  cargo run --release -- --script assets/lisp/main3d.lisp --capture demo/frames3d --every 6 --frames 540 --fast

РЕЖИМЫ:
  --script FILE   точка входа Lisp (по умолчанию assets/lisp/main.lisp)
  --ascii         рендер в терминал (ANSI)
  --stdin-keys    клавиши из stdin (raw mode; строчные — нажать, заглавные — отпустить)
  --input FILE    сценарий ввода: строки «ВРЕМЯ КЛАВИША down|up»
  --capture DIR   писать кадры PNG в DIR
  --every N       каждый N-й кадр (по умолчанию 3)
  --frames N      остановиться после N кадров
  --fps F         шаг симуляции (по умолчанию 30)
  --fast          без привязки к реальному времени
  --width W --height H   стартовый размер экрана
  --bench         бенчмарк SIMD-слоя + 2D-кадра + 3D-шейдинга (этап 7)
  --bench-parse   бенчмарк токенизатора/парсера: старый посимвольный reader vs
                  новый SIMD-сканер (src/simd.rs) на реальном корпусе
                  assets/lisp/*.lisp; оба пути обязаны давать одинаковые AST
  --wgpu          GPU-бэкенд за фасадом HAL (фича `wgpu`): offscreen-рендер +
                  readback в фреймбуфер; без адаптера — откат на программный
                  растр с предупреждением (все скрипты работают как раньше)
  --eval EXPR     вычислить выражение Lisp и выйти

РЕДАКТОР И AI (v0.3):
  --serve         веб-редактор аля Unity: иерархия, инспектор, REPL,
                  живой вид, ассеты с hot reload (http://127.0.0.1:7300)
  --port N        порт для --serve (по умолчанию 7300)
  --mcp           MCP-сервер (Model Context Protocol, JSON-RPC 2.0, stdio):
                  ИИ-агент управляет движком — scene/step/eval/screenshot";

pub fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--script" => args.script = it.next().ok_or("--script: нужен путь")?,
            "--ascii" => args.ascii = true,
            "--capture" => args.capture = Some(it.next().ok_or("--capture: нужен каталог")?),
            "--every" => {
                args.every = it
                    .next()
                    .ok_or("--every: нужно N")?
                    .parse()
                    .map_err(|_| "--every: не число")?
            }
            "--frames" => {
                let n: u64 = it
                    .next()
                    .ok_or("--frames: нужно N")?
                    .parse()
                    .map_err(|_| "--frames: не число")?;
                args.frames = if n == 0 { u64::MAX } else { n };
            }
            "--fps" => {
                args.fps = it
                    .next()
                    .ok_or("--fps: нужно F")?
                    .parse()
                    .map_err(|_| "--fps: не число")?
            }
            "--width" => {
                args.width = it
                    .next()
                    .ok_or("--width: нужно W")?
                    .parse()
                    .map_err(|_| "--width: не число")?
            }
            "--height" => {
                args.height = it
                    .next()
                    .ok_or("--height: нужно H")?
                    .parse()
                    .map_err(|_| "--height: не число")?
            }
            "--input" => args.input = Some(it.next().ok_or("--input: нужен файл")?),
            "--stdin-keys" => args.stdin_keys = true,
            "--fast" => args.fast = true,
            "--bench" => args.bench = true,
            "--bench-parse" => args.bench_parse = true,
            "--bench-parse-allocs" => args.bench_parse_allocs = true,
            "--wgpu" => args.wgpu = true,
            "--eval" => args.eval_code = Some(it.next().ok_or("--eval: нужно выражение")?),
            "--serve" => args.serve = true,
            "--port" => {
                args.port = it
                    .next()
                    .ok_or("--port: нужно N")?
                    .parse()
                    .map_err(|_| "--port: не число")?
            }
            "--mcp" => args.mcp = true,
            "--record" => args.record = Some(it.next().ok_or("--record: нужен путь")?),
            "--replay" => args.replay = Some(it.next().ok_or("--replay: нужен путь")?),
            "--help" | "-h" => {
                println!("{}", USAGE);
                std::process::exit(0);
            }
            other => return Err(format!("неизвестный флаг: {}", other)),
        }
    }
    Ok(args)
}
