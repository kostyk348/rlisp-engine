// src/editor.rs — v0.3: редактор аля Unity поверх движка. Ноль зависимостей:
// std::net::TcpListener + std::thread. Философия та же: сервер — «железо»
// (как PNG-писатель), вся семантика сцены — Lisp (assets/lisp/editor.lisp,
// функция (editor-scene) вызывается ядром каждый кадр, если определена).
//
// Мост Bridge: игровая нить публикует кадр/сцену/лог; HTTP-нить отвечает на
// запросы браузера. Lisp-код из редактора исполняется игровой нитью в начале
// кадра (командная очередь) — гонок с интерпретатором нет.
//
// API:
//   GET  /                  — страница редактора (assets/editor/index.html)
//   GET  /frame.png         — текущий кадр (PNG, кодируется на лету)
//   GET  /api/state?since=N — кадр, fps, сцена, результаты eval, лог, ассеты,
//                            камера+префабы (этап 6: хуки editor-camera/prefab-list)
//   GET  /api/file?path=    — текст Lisp-файла (только внутри assets/lisp)
//   POST /api/eval {code}   — исполнить Lisp в живом окружении движка
//   POST /api/save {path,content} — сохранить файл (движок сам подхватит hot reload)
//   POST /api/key  {key,down}     — событие клавиатуры в игру
//   POST /api/ctl  {cmd}          — pause | resume | step | reload
//   POST /api/save-scene {path}   — сериализовать сцену в Lisp-файл (этап 6)

use crate::interp::{env_get, fmt_value, parse_program, CoreCtx, Gc, GlobalCell, Value};
use crate::json::Json;
use crate::png;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub struct Bridge {
    pub fb: Mutex<Vec<u8>>,
    pub fb_wh: Mutex<(usize, usize)>,
    pub frame: AtomicU64,
    pub fps: Mutex<f64>,
    pub drawcalls: AtomicU64,
    pub scene: Mutex<String>,
    pub cmds: Mutex<Vec<(u64, String)>>,
    pub results: Mutex<Vec<(u64, String)>>,
    pub next_id: AtomicU64,
    pub keys: Mutex<Vec<(String, bool)>>,
    pub paused: AtomicBool,
    pub step_req: AtomicBool,
    pub reload_req: AtomicBool,
    pub log: Mutex<VecDeque<(u64, String)>>,
    pub log_seq: AtomicU64,
    pub assets: Mutex<Vec<String>>,
    /// Этап 6: камера игры — JSON от хука (editor-camera), «null» если хука нет.
    pub camera: Mutex<String>,
    /// Этап 6: префабы игры — JSON от хука (prefab-list), «null» если хука нет.
    pub prefabs: Mutex<String>,
}

impl Bridge {
    pub fn new() -> Arc<Bridge> {
        Arc::new(Bridge {
            fb: Mutex::new(Vec::new()),
            fb_wh: Mutex::new((0, 0)),
            frame: AtomicU64::new(0),
            fps: Mutex::new(0.0),
            drawcalls: AtomicU64::new(0),
            scene: Mutex::new("[]".into()),
            cmds: Mutex::new(Vec::new()),
            results: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            keys: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            step_req: AtomicBool::new(false),
            reload_req: AtomicBool::new(false),
            log: Mutex::new(VecDeque::new()),
            log_seq: AtomicU64::new(0),
            assets: Mutex::new(Vec::new()),
            camera: Mutex::new("null".into()),
            prefabs: Mutex::new("null".into()),
        })
    }

    pub fn log_line(&self, s: String) {
        let mut q = self.log.lock().unwrap();
        let seq = self.log_seq.fetch_add(1, Ordering::Relaxed);
        q.push_back((seq, s));
        while q.len() > 200 {
            q.pop_front();
        }
    }
}

fn http_res(stream: &mut TcpStream, code: &str, ctype: &str, body: Vec<u8>) {
    let head = format!(
        "HTTP/1.1 {} \r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        code,
        ctype,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn http_json(stream: &mut TcpStream, j: &Json) {
    http_res(
        stream,
        "200 OK",
        "application/json; charset=utf-8",
        j.dump().into_bytes(),
    );
}

fn http_err(stream: &mut TcpStream, _code: &str, msg: &str) {
    http_json(stream, &Json::obj(vec![("error", Json::str(msg))]));
}

fn body_field(body: &str, field: &str) -> Option<String> {
    crate::json::parse(body)
        .ok()?
        .get(field)?
        .as_str()
        .map(|s| s.to_string())
}

/// Достаём "?a=b" из пути запроса.
fn query(raw: &str) -> BTreeMapS {
    let mut m = BTreeMapS::default();
    if let Some(qpos) = raw.find('?') {
        for pair in raw[qpos + 1..].split('&') {
            let mut kv = pair.splitn(2, '=');
            let k = kv.next().unwrap_or("").to_string();
            let v = kv.next().unwrap_or("").to_string();
            m.0.insert(k, url_decode(&v));
        }
    }
    m
}

#[derive(Default)]
struct BTreeMapS(std::collections::BTreeMap<String, String>);

impl BTreeMapS {
    fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|s| s.as_str())
    }
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn safe_lisp_path(root: &std::path::Path, p: &str) -> Option<std::path::PathBuf> {
    let path = root.join(p);
    let canon = std::fs::canonicalize(&path).ok()?;
    let root_canon = std::fs::canonicalize(root).ok()?;
    if canon.starts_with(&root_canon) && canon.extension().map(|e| e == "lisp").unwrap_or(false) {
        Some(canon)
    } else {
        None
    }
}

fn serve(mut stream: TcpStream, bridge: Arc<Bridge>, root: std::path::PathBuf) {
    let peer = stream.try_clone();
    let mut reader = BufReader::new(match peer {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();

    // заголовки
    let mut content_len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        let lower = h.to_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_len = v.trim().parse().unwrap_or(0);
        }
    }
    if content_len > 8 * 1024 * 1024 {
        http_err(&mut stream, "413 Too Large", "тело слишком большое");
        return;
    }
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        reader.read_exact(&mut body).ok();
    }
    let body_str = String::from_utf8_lossy(&body).into_owned();

    let path = raw_path.split('?').next().unwrap_or("/").to_string();
    let q = query(&raw_path);

    match (method.as_str(), path.as_str()) {
        ("GET", "/") => {
            let html = include_str!("../assets/editor/index.html");
            http_res(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                html.as_bytes().to_vec(),
            );
        }
        ("GET", "/frame.png") => {
            let fb = bridge.fb.lock().unwrap().clone();
            let (w, h) = *bridge.fb_wh.lock().unwrap();
            if w == 0 || fb.is_empty() {
                http_err(&mut stream, "503", "кадра ещё нет");
                return;
            }
            match png::encode_png(&fb, w, h) {
                Ok(bytes) => http_res(&mut stream, "200 OK", "image/png", bytes),
                Err(e) => http_err(&mut stream, "500", &e),
            }
        }
        ("GET", "/api/state") => {
            let since: u64 = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
            let results: Vec<(u64, String)> = bridge
                .results
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| *id > since)
                .cloned()
                .collect();
            let max_id = results.last().map(|(id, _)| *id).unwrap_or(since);
            let log: Vec<Json> = bridge
                .log
                .lock()
                .unwrap()
                .iter()
                .map(|(seq, line)| Json::Arr(vec![Json::int(*seq as i64), Json::str(line)]))
                .collect();
            let scene = bridge.scene.lock().unwrap().clone();
            let assets = bridge.assets.lock().unwrap().clone();
            let camera = bridge.camera.lock().unwrap().clone();
            let prefabs = bridge.prefabs.lock().unwrap().clone();
            let j = Json::obj(vec![
                (
                    "frame",
                    Json::int(bridge.frame.load(Ordering::Relaxed) as i64),
                ),
                ("fps", Json::num(*bridge.fps.lock().unwrap())),
                (
                    "drawcalls",
                    Json::int(bridge.drawcalls.load(Ordering::Relaxed) as i64),
                ),
                ("paused", Json::Bool(bridge.paused.load(Ordering::Relaxed))),
                ("last_result", Json::int(max_id as i64)),
                (
                    "results",
                    Json::Arr(
                        results
                            .iter()
                            .map(|(id, r)| {
                                Json::obj(vec![
                                    ("id", Json::int(*id as i64)),
                                    ("result", Json::str(r)),
                                ])
                            })
                            .collect(),
                    ),
                ),
                ("scene", Json::Str(scene)),
                ("log", Json::Arr(log)),
                (
                    "assets",
                    Json::Arr(assets.iter().map(|a| Json::str(a)).collect()),
                ),
                // этап 6: камера для гизмо и префабы для drag-n-drop
                ("camera", Json::Str(camera)),
                ("prefabs", Json::Str(prefabs)),
            ]);
            http_json(&mut stream, &j);
        }
        ("GET", "/api/file") => {
            let p = q.get("path").unwrap_or("").to_string();
            match safe_lisp_path(&root, &p) {
                Some(canon) => match std::fs::read_to_string(&canon) {
                    Ok(content) => http_json(
                        &mut stream,
                        &Json::obj(vec![("content", Json::str(&content))]),
                    ),
                    Err(e) => http_err(&mut stream, "404", &format!("{}", e)),
                },
                None => http_err(&mut stream, "403", "только .lisp внутри assets/lisp"),
            }
        }
        ("POST", "/api/eval") => {
            let code = body_field(&body_str, "code");
            let id = bridge.next_id.fetch_add(1, Ordering::Relaxed);
            match code {
                Some(c) => {
                    bridge.cmds.lock().unwrap().push((id, c.clone()));
                    bridge.log_line(format!(
                        "[eval #{id}] {}",
                        c.lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(80)
                            .collect::<String>()
                    ));
                    http_json(&mut stream, &Json::obj(vec![("id", Json::int(id as i64))]));
                }
                None => http_err(&mut stream, "400", "нужно поле code"),
            }
        }
        ("POST", "/api/save") => {
            let parsed = crate::json::parse(&body_str);
            match parsed {
                Ok(j) => {
                    let p = j
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let content = j
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    match safe_lisp_path(&root, &p) {
                        Some(canon) => match std::fs::write(&canon, content.as_bytes()) {
                            Ok(_) => {
                                bridge.log_line(format!("[save] {} — hot reload подхватит сам", p));
                                http_json(&mut stream, &Json::obj(vec![("ok", Json::Bool(true))]));
                            }
                            Err(e) => http_err(&mut stream, "500", &format!("{}", e)),
                        },
                        None => http_err(&mut stream, "403", "только .lisp внутри assets/lisp"),
                    }
                }
                Err(e) => http_err(&mut stream, "400", &e),
            }
        }
        ("POST", "/api/key") => {
            if let Ok(j) = crate::json::parse(&body_str) {
                let key = j
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let down = j.get("down").and_then(|v| v.as_bool()).unwrap_or(true);
                if !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase()) {
                    bridge.keys.lock().unwrap().push((key, down));
                }
            }
            http_json(&mut stream, &Json::obj(vec![("ok", Json::Bool(true))]));
        }
        ("POST", "/api/ctl") => {
            let cmd = body_field(&body_str, "cmd");
            match cmd.as_deref() {
                Some("pause") => bridge.paused.store(true, Ordering::Relaxed),
                Some("resume") => bridge.paused.store(false, Ordering::Relaxed),
                Some("step") => bridge.step_req.store(true, Ordering::Relaxed),
                Some("reload") => bridge.reload_req.store(true, Ordering::Relaxed),
                _ => {
                    http_err(&mut stream, "400", "cmd: pause|resume|step|reload");
                    return;
                }
            }
            http_json(&mut stream, &Json::obj(vec![("ok", Json::Bool(true))]));
        }
        // Этап 6: сохранение сцены — игровая нить сериализует (scene-entities)
        // в Lisp-файл через (editor-save-scene), файл пишется нативом file-write.
        ("POST", "/api/save-scene") => {
            let raw = body_field(&body_str, "path").unwrap_or_else(|| "scene_saved.lisp".into());
            // только имя файла внутри assets/lisp — без каталогов и traversal
            let fname = std::path::Path::new(&raw)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "scene_saved.lisp".into());
            let id = bridge.next_id.fetch_add(1, Ordering::Relaxed);
            let code = format!("(editor-save-scene \"{}\")", fname);
            bridge.cmds.lock().unwrap().push((id, code));
            bridge.log_line(format!("[save-scene #{id}] {fname}"));
            http_json(&mut stream, &Json::obj(vec![("id", Json::int(id as i64))]));
        }
        _ => http_err(&mut stream, "404", "нет такого маршрута"),
    }
}

pub fn start_server(
    bridge: Arc<Bridge>,
    port: u16,
    root: std::path::PathBuf,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    let b = bridge.clone();
                    let r = root.clone();
                    std::thread::spawn(move || serve(stream, b, r));
                }
                Err(_) => continue,
            }
        }
    });
    Ok(())
}

static BRIDGE: std::sync::OnceLock<Arc<Bridge>> = std::sync::OnceLock::new();

pub fn install_bridge(b: Arc<Bridge>) {
    let _ = BRIDGE.set(b);
}

fn bridge() -> Option<&'static Arc<Bridge>> {
    BRIDGE.get()
}

/// Натив (editor-log msg) — Lisp пишет в консоль редактора (или в stderr).
pub fn register_editor(env: Gc) {
    crate::interp::env_define(
        env,
        "editor-log",
        Value::Native(
            Rc::new(move |args: &[Value]| {
                let parts: Vec<String> = args.iter().map(fmt_value).collect();
                let line = parts.join(" ");
                match bridge() {
                    Some(b) => b.log_line(line),
                    None => eprintln!("[lisp] {}", line),
                }
                Ok(Value::Nil)
            }),
            "editor-log".to_string(),
        ),
    );
}

/// Исполнить Lisp-строку в живом окружении (команды редактора/MCP).
pub fn eval_string(code: &str, cell: &GlobalCell) -> Result<Value, String> {
    let prog = parse_program(code)?;
    // формы (могут нести Cons-литералы) и последний результат переживают
    // сборки на границах форм — pin-корни
    let _pin = crate::gc::pin(prog.clone());
    let g = *cell.borrow();
    let mut last = Value::Nil;
    let pin = crate::gc::pin(vec![last.clone()]);
    for e in prog {
        last = crate::interp::eval(&e, g, 0)?;
        crate::gc::pin_replace(&pin, vec![last.clone()]);
        crate::gc::maybe_collect(&[g], &[]);
    }
    drop(pin);
    Ok(last)
}

/// Публикация сцены: (editor-scene) из Lisp → JSON-строка в Bridge.
pub fn publish_scene(cell: &GlobalCell, bridge: &Arc<Bridge>) {
    let g = *cell.borrow();
    if let Some(f) = env_get(g, "editor-scene") {
        match crate::interp::apply(&f, &[], 0) {
            Ok(v) => {
                *bridge.scene.lock().unwrap() = crate::json::value_to_json(&v).dump();
            }
            Err(e) => bridge.log_line(format!("[scene] ошибка: {}", e)),
        }
    }
}

/// Этап 6: публикация хуков камеры/префабов — (editor-camera) и (prefab-list),
/// если игра их объявила. Нет хука — в Bridge остаётся «null», редактор честно
/// прячет гизмо/префабы.
pub fn publish_extras(cell: &GlobalCell, bridge: &Arc<Bridge>) {
    let g = *cell.borrow();
    if let Some(f) = env_get(g, "editor-camera") {
        if let Ok(v) = crate::interp::apply(&f, &[], 0) {
            *bridge.camera.lock().unwrap() = crate::json::value_to_json(&v).dump();
        }
    }
    if let Some(f) = env_get(g, "prefab-list") {
        if let Ok(v) = crate::interp::apply(&f, &[], 0) {
            *bridge.prefabs.lock().unwrap() = crate::json::value_to_json(&v).dump();
        }
    }
}

/// Игровая нить: разбор команд редактора (eval/keys/ctl-запросы reload).
/// Возвращает true, если была команда reload.
pub fn drain_editor_cmds(
    bridge: &Arc<Bridge>,
    cell: &GlobalCell,
    halrc: &Rc<RefCell<crate::hal::Hal>>,
) -> bool {
    let mut reload = false;
    if bridge.reload_req.swap(false, Ordering::Relaxed) {
        reload = true;
    }
    let cmds: Vec<(u64, String)> = std::mem::take(&mut *bridge.cmds.lock().unwrap());
    for (id, code) in cmds {
        let res = eval_string(&code, cell);
        let text = match res {
            Ok(v) => fmt_value(&v),
            Err(e) => format!("ОШИБКА: {}", e),
        };
        bridge.results.lock().unwrap().push((id, text.clone()));
        let mut rq = bridge.results.lock().unwrap();
        while rq.len() > 64 {
            rq.remove(0);
        }
        bridge.log_line(format!(
            "[#{}] {}",
            id,
            text.lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>()
        ));
    }
    let keys: Vec<(String, bool)> = std::mem::take(&mut *bridge.keys.lock().unwrap());
    if !keys.is_empty() {
        let mut hal = halrc.borrow_mut();
        for (k, down) in keys {
            let ev = if down { "key-down" } else { "key-up" };
            hal.queue
                .push(Value::List(vec![Value::Sym(ev.into()), Value::Str(k)]));
        }
    }
    reload
}

/// Контекст для MCP/eval-инструментов.
pub struct EngineHandle {
    pub ctx: CoreCtx,
    pub cell: GlobalCell,
    pub halrc: Rc<RefCell<crate::hal::Hal>>,
}

/// Шаг кадра для MCP-режима (update → draw → frame-end).
pub fn step_frame(h: &EngineHandle, dt: f64) -> Result<(), String> {
    let g = *h.cell.borrow();
    if let Some(f) = env_get(g, "update") {
        crate::interp::apply(&f, &[Value::Float(dt)], 0)?;
    }
    if let Some(f) = env_get(g, "draw") {
        crate::interp::apply(&f, &[], 0)?;
    }
    if let Some(f) = env_get(g, "frame-end") {
        let _ = crate::interp::apply(&f, &[], 0);
    }
    // Этап 2: граница форм MCP-цикла — авто-сборка GC
    crate::gc::maybe_collect(&[g], &[]);
    Ok(())
}

// ——————————————————————————— тесты этапа 6 ———————————————————————————

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hal::OutputMode;
    use std::path::PathBuf;

    /// Этап 6: round-trip сохранения сцены. Заглушка игры даёт 3 сущности
    /// (2 врага + игрок, пуля — переходящая), (editor-save-scene) сериализует
    /// их в Lisp-файл, читатель парсит файл назад, (load) восстанавливает —
    /// те же сущности и позиции.
    #[test]
    fn save_scene_roundtrip() {
        let halrc = Rc::new(RefCell::new(crate::hal::Hal::new(OutputMode::Silent, 8, 8)));
        let (ctx, cell) = crate::make_context(halrc, PathBuf::from("."));
        let root = PathBuf::from(".");
        crate::interp::load_file(&root.join("assets/lisp/core.lisp"), &ctx).unwrap();
        crate::interp::load_file(&root.join("assets/lisp/editor.lisp"), &ctx).unwrap();

        // заглушки: mcp-spawn-at пишет в *spawned*, игрок — обычная таблица
        eval_string(
            "(define *spawned* '())\n\
             (define *stub-player* (make-table))\n\
             (define (game-spawn-at kind x z)\n\
               (set! *spawned* (cons (list kind x z) *spawned*))\n\
               (length *spawned*))",
            &cell,
        )
        .unwrap();
        // сцена: 2 врага + игрок + переходящая пуля (пуля не сериализуется)
        eval_string(
            "(define (scene-entities)\n\
               (list (obj 'id 0 'type \"enemy\" 'name \"brute\" 'ref \"(game-entity 0)\" 'x 11.5 'z -3.0 'hp 3)\n\
                     (obj 'id 1 'type \"enemy\" 'name \"grunt\" 'ref \"(game-entity 1)\" 'x 4.25 'z 7.5 'hp 1)\n\
                     (obj 'id 0 'type \"player\" 'name \"Игрок\" 'ref \"*stub-player*\" 'x 0.0 'z 0.0)\n\
                     (obj 'type \"bullet\" 'name \"Пуля\" 'x 1.0 'z 1.0)))",
            &cell,
        )
        .unwrap();

        // сериализация во временный файл: 3 формы (2 врага + игрок)
        let path = std::env::temp_dir().join("rlisp_scene_roundtrip.lisp");
        let res = eval_string(
            &format!("(editor-save-scene \"{}\")", path.display()),
            &cell,
        )
        .unwrap();
        assert_eq!(fmt_value(&res), "3");

        // файл парсится назад читателем — формы восстановления с позициями
        let src = std::fs::read_to_string(&path).unwrap();
        let prog = parse_program(&src).unwrap();
        assert_eq!(prog.len(), 3);
        assert_eq!(
            fmt_value(&prog[0]),
            "(mcp-spawn-at (quote brute) 11.5 -3.0)"
        );
        assert_eq!(fmt_value(&prog[1]), "(mcp-spawn-at (quote grunt) 4.25 7.5)");
        assert_eq!(
            fmt_value(&prog[2]),
            "(begin (table-set! *stub-player* (quote x) 0.0) (table-set! *stub-player* (quote z) 0.0))"
        );

        // round-trip: (load) файла в живом окружении → те же сущности и позиции
        eval_string("(set! *spawned* '())", &cell).unwrap();
        eval_string(&format!("(load \"{}\")", path.display()), &cell).unwrap();
        let spawned = eval_string("*spawned*", &cell).unwrap();
        assert_eq!(fmt_value(&spawned), "((grunt 4.25 7.5) (brute 11.5 -3.0))");
        let px = eval_string("(table-get *stub-player* 'x 99)", &cell).unwrap();
        assert_eq!(fmt_value(&px), "0.0");
        let _ = std::fs::remove_file(&path);
    }

    /// Хуки камеры/префабов публикуются только если игра их объявила:
    /// нет хуков — в Bridge остаётся «null», редактор прячет гизмо/префабы.
    #[test]
    fn publish_extras_missing_hooks_stay_null() {
        let halrc = Rc::new(RefCell::new(crate::hal::Hal::new(OutputMode::Silent, 8, 8)));
        let (_ctx, cell) = crate::make_context(halrc, PathBuf::from("."));
        let bridge = Bridge::new();
        // убедимся, что в пустом окружении хуков нет
        assert!(env_get(*cell.borrow(), "editor-camera").is_none());
        publish_extras(&cell, &bridge);
        assert_eq!(*bridge.camera.lock().unwrap(), "null");
        assert_eq!(*bridge.prefabs.lock().unwrap(), "null");
    }

    /// /api/save-scene не пускает пути наружу: остаётся только имя файла.
    #[test]
    fn save_scene_path_is_sanitized() {
        // проверяем логику обрезки пути, как в обработчике
        let raw = "../../etc/passwd.lisp";
        let fname = std::path::Path::new(raw)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(fname, "passwd.lisp");
    }
}
