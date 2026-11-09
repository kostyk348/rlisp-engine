// src/mcp.rs — v0.3: MCP-сервер движка (Model Context Protocol, JSON-RPC 2.0
// поверх stdio, newline-delimited). «AI Ready»: ИИ-агент подключается к живому
// движку и управляет им — смотрит сцену, спавнит сущности, правит свойства,
// исполняет Lisp, шагает симуляцию, получает СКРИНШОТЫ (image/png, base64).
//
// Запуск: rlisp-engine --mcp --script assets/lisp/main3d.lisp
// Протокольная обвязка — Rust (как png.rs: байты, не семантика); инструменты
// вызывают Lisp (editor.lisp/game3d.lisp) — сцена и сущности живут на Lisp.
//
// Этап 2 — потоки: протокол обслуживают ФОНОВЫЕ потоки, интерпретатор
// остаётся однопоточным владельцем Lisp-состояния:
//
//   поток-reader (stdin)                игровая нить (владелец interp)     поток-writer (stdout)
//   ────────────────────                ──────────────────────────────     ─────────────────────
//   строка → JSON → McpRequest  ─mpsc▶  mcp_engine_loop:                   ◀mpsc─ строка ответа
//   (parse-ошибка → ответ сразу)        между кадрами разбирает очередь,   println + flush
//                                       инструменты применяет здесь же
//
// Потоки общаются ТОЛЬКО каналами: Values и Hal никогда не покидают игровую
// нить, блокировок на состояниях интерпретатора нет — гонок по построению.
// --mcp поведение совместимо: тот же протокол, те же ответы; при пустой
// очереди движок просто ждёт следующий запрос (recv — «пауза между кадрами»).

use crate::editor::{eval_string, step_frame, EngineHandle};
use crate::interp::{env_get, fmt_value, load_file, GlobalCell};
use crate::json::Json;
use crate::png;
use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn tool_def(name: &str, desc: &str, props: Vec<(&str, &str)>) -> Json {
    let mut p = std::collections::BTreeMap::new();
    for (k, ty) in props {
        p.insert(
            k.to_string(),
            Json::obj(vec![("type", Json::str(ty)), ("description", Json::str(k))]),
        );
    }
    Json::obj(vec![
        ("name", Json::str(name)),
        ("description", Json::str(desc)),
        (
            "inputSchema",
            Json::obj(vec![
                ("type", Json::str("object")),
                ("properties", Json::Obj(p)),
            ]),
        ),
    ])
}

fn text_content(s: &str) -> Json {
    Json::obj(vec![
        (
            "content",
            Json::Arr(vec![Json::obj(vec![
                ("type", Json::str("text")),
                ("text", Json::str(s)),
            ])]),
        ),
        ("isError", Json::Bool(false)),
    ])
}

fn err_content(s: &str) -> Json {
    Json::obj(vec![
        (
            "content",
            Json::Arr(vec![Json::obj(vec![
                ("type", Json::str("text")),
                ("text", Json::str(s)),
            ])]),
        ),
        ("isError", Json::Bool(true)),
    ])
}

/// Запрос, разобранный reader-потоком: только данные протокола, никакого
/// доступа к интерпретатору. Send по построению (Json — owned-данные).
pub struct McpRequest {
    pub id: Json,
    pub method: String,
    pub params: Json,
    pub is_notification: bool,
}

pub struct McpState {
    pub h: EngineHandle,
    pub frames: u64,
    pub fps: f64,
    pub dt: f64,
}

fn fmt_get(cell: &GlobalCell, name: &str) -> String {
    let g = *cell.borrow();
    env_get(g, name)
        .map(|v| fmt_value(&v))
        .unwrap_or_else(|| "?".into())
}

fn tool_status(state: &McpState) -> Json {
    let hal = state.h.halrc.borrow();
    let mut fields: Vec<(String, Json)> = vec![
        ("frames".into(), Json::int(state.frames as i64)),
        ("fps_headless".into(), Json::num(state.fps)),
        ("screen".into(), Json::str(&format!("{}x{}", hal.w, hal.h))),
        (
            "simd".into(),
            Json::str(crate::simd::level_name(crate::simd::detect())),
        ),
        (
            "score".into(),
            Json::str(&fmt_get(&state.h.cell, "*score*")),
        ),
        ("wave".into(), Json::str(&fmt_get(&state.h.cell, "*wave*"))),
        (
            "kills".into(),
            Json::str(&fmt_get(&state.h.cell, "*kills*")),
        ),
        (
            "game_over".into(),
            Json::str(&fmt_get(&state.h.cell, "*game-over*")),
        ),
        ("drawcalls".into(), Json::int(hal.r3.drawn as i64)),
        ("culled".into(), Json::int(hal.r3.culled as i64)),
        ("textures".into(), Json::int(hal.tex.len() as i64)),
    ];
    drop(hal);
    if fmt_get(&state.h.cell, "*enemies*") != "?" {
        if let Ok(v) = eval_string("(length (table-entries *enemies*))", &state.h.cell) {
            fields.push(("enemies".into(), Json::str(&fmt_value(&v))));
        }
    }
    Json::Obj(fields.into_iter().collect())
}

fn tool_scene(state: &McpState) -> String {
    let g = *state.h.cell.borrow();
    if let Some(f) = env_get(g, "editor-scene") {
        match crate::interp::apply(&f, &[], 0) {
            Ok(v) => return crate::json::value_to_json(&v).dump(),
            Err(e) => return format!("{{\"error\":\"{}\"}}", e.replace('"', "'")),
        }
    }
    "[]".into()
}

fn call_tool(state: &mut McpState, name: &str, a: &Json) -> Json {
    match name {
        "status" => text_content(&tool_status(state).dump()),
        "scene_get" => text_content(&tool_scene(state)),
        "lisp_eval" => {
            let code = a.get("code").and_then(|v| v.as_str()).unwrap_or("");
            if code.is_empty() {
                err_content("нужен code")
            } else {
                match eval_string(code, &state.h.cell) {
                    Ok(v) => text_content(&fmt_value(&v)),
                    Err(e) => err_content(&format!("ошибка: {}", e)),
                }
            }
        }
        "entity_spawn" => {
            let kind = a.get("kind").and_then(|v| v.as_str()).unwrap_or("grunt");
            let x = a.get("x").and_then(|v| v.as_f64());
            let z = a.get("z").and_then(|v| v.as_f64());
            let code = match (x, z) {
                (Some(x), Some(z)) => format!("(mcp-spawn-at '{} {} {})", kind, x, z),
                _ => format!("(mcp-spawn '{}", kind),
            };
            match eval_string(&code, &state.h.cell) {
                Ok(v) => text_content(&format!("создано: {}", fmt_value(&v))),
                Err(e) => err_content(&format!("ошибка: {}", e)),
            }
        }
        "entity_set" => {
            let id = a.get("id").and_then(|v| v.as_f64()).unwrap_or(-1.0);
            let prop = a.get("prop").and_then(|v| v.as_str()).unwrap_or("");
            let val = a.get("value").cloned().unwrap_or(Json::Num(0.0));
            if id < 0.0 || prop.is_empty() {
                err_content("нужны id и prop")
            } else {
                let lv = crate::json::json_to_value(&val);
                let code = format!("(mcp-set {} '{}' {})", id as i64, prop, fmt_value(&lv));
                match eval_string(&code, &state.h.cell) {
                    Ok(_) => text_content(&format!("{}.{} = {}", id as i64, prop, fmt_value(&lv))),
                    Err(e) => err_content(&format!("ошибка: {}", e)),
                }
            }
        }
        "entity_remove" => {
            let id = a.get("id").and_then(|v| v.as_f64()).unwrap_or(-1.0);
            match eval_string(&format!("(mcp-remove {})", id as i64), &state.h.cell) {
                Ok(_) => text_content(&format!("удалён {}", id as i64)),
                Err(e) => err_content(&format!("ошибка: {}", e)),
            }
        }
        "step" => {
            let n = a
                .get("frames")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
                .clamp(1.0, 600.0) as u64;
            let t0 = std::time::Instant::now();
            for _ in 0..n {
                if let Err(e) = step_frame(&state.h, state.dt) {
                    return err_content(&format!("кадр {}: {}", state.frames, e));
                }
                state.frames += 1;
            }
            state.fps = n as f64 / t0.elapsed().as_secs_f64().max(1e-9);
            text_content(&format!(
                "выполнено {} кадров ({:.1} fps headless)",
                n, state.fps
            ))
        }
        "screenshot" => {
            let hal = state.h.halrc.borrow();
            match png::encode_png(&hal.fb, hal.w, hal.h) {
                Ok(bytes) => {
                    drop(hal);
                    let s = b64(&bytes);
                    Json::obj(vec![
                        (
                            "content",
                            Json::Arr(vec![Json::obj(vec![
                                ("type", Json::str("image")),
                                ("data", Json::Str(s)),
                                ("mimeType", Json::str("image/png")),
                            ])]),
                        ),
                        ("isError", Json::Bool(false)),
                    ])
                }
                Err(e) => err_content(&format!("png: {}", e)),
            }
        }
        "hot_reload" => {
            let files: Vec<std::path::PathBuf> = state.h.ctx.registry.borrow().clone();
            let mut mtimes = crate::snapshot_mtimes(&files);
            match crate::reload_all(&state.h.ctx, &state.h.halrc, &mut mtimes) {
                Ok(n) => text_content(&format!(
                    "перезагружено, {} persistent-значений восстановлено",
                    n
                )),
                Err(e) => err_content(&format!("ошибка: {} — работаем на старом окружении", e)),
            }
        }
        "key" => {
            let k = a
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let down = a.get("down").and_then(|v| v.as_bool()).unwrap_or(true);
            if k.is_empty() || !k.chars().all(|c| c.is_ascii_lowercase()) {
                err_content("нужна клавиша a-z")
            } else {
                let ev = if down { "key-down" } else { "key-up" };
                state
                    .h
                    .halrc
                    .borrow_mut()
                    .queue
                    .push(crate::interp::Value::List(vec![
                        crate::interp::Value::Sym(ev.into()),
                        crate::interp::Value::Str(k.clone()),
                    ]));
                text_content(&format!("{} {}", k, if down { "down" } else { "up" }))
            }
        }
        other => err_content(&format!("неизвестный инструмент: {}", other)),
    }
}

fn tools_list() -> Vec<Json> {
    vec![
        tool_def(
            "status",
            "Состояние движка: кадры, fps, счёт, волна, враги, draw calls",
            vec![],
        ),
        tool_def(
            "scene_get",
            "Сцена в JSON: игрок, враги, пули, эффекты с координатами",
            vec![],
        ),
        tool_def(
            "lisp_eval",
            "Исполнить код Lisp в живом окружении движка (полный доступ)",
            vec![("code", "string")],
        ),
        tool_def(
            "entity_spawn",
            "Заспавнить врага: kind = grunt|brute",
            vec![("kind", "string"), ("x", "number"), ("z", "number")],
        ),
        tool_def(
            "entity_set",
            "Свойство сущности: id, prop (x|z|hp|...), value",
            vec![("id", "number"), ("prop", "string"), ("value", "any")],
        ),
        tool_def(
            "entity_remove",
            "Удалить врага по id",
            vec![("id", "number")],
        ),
        tool_def(
            "step",
            "Прошагать симуляцию N кадров (1..600)",
            vec![("frames", "number")],
        ),
        tool_def(
            "screenshot",
            "Скриншот экрана движка: PNG base64 (image content)",
            vec![],
        ),
        tool_def(
            "hot_reload",
            "Перечитать .lisp-файлы (состояние defpersistent переносится)",
            vec![],
        ),
        tool_def(
            "key",
            "Событие клавиатуры: key a-z, down bool",
            vec![("key", "string"), ("down", "boolean")],
        ),
    ]
}

fn resp_line(id: Json, result: Json) -> String {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id),
        ("result", result),
    ])
    .dump()
}

fn err_line(id: Json, code: i64, message: &str) -> String {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id),
        (
            "error",
            Json::obj(vec![
                ("code", Json::int(code)),
                ("message", Json::str(message)),
            ]),
        ),
    ])
    .dump()
}

/// Обработать один запрос на игровой нити (владелец интерпретатора).
/// Ответ — строка JSON в канал writer-потока.
fn handle_request(state: &mut McpState, req: McpRequest, resp: &Sender<String>) {
    let McpRequest {
        id,
        method,
        params,
        is_notification,
    } = req;
    let line = match method.as_str() {
        "initialize" => resp_line(
            id,
            Json::obj(vec![
                ("protocolVersion", Json::str("2024-11-05")),
                (
                    "capabilities",
                    Json::obj(vec![("tools", Json::obj(vec![]))]),
                ),
                (
                    "serverInfo",
                    Json::obj(vec![
                        ("name", Json::str("rlisp-engine")),
                        ("version", Json::str("0.3.0")),
                        ("title", Json::str("RLISP Game Engine — Lisp-машина")),
                    ]),
                ),
            ]),
        ),
        "notifications/initialized" | "notifications/cancelled" => return,
        "tools/list" => resp_line(id, Json::obj(vec![("tools", Json::Arr(tools_list()))])),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or(Json::obj(vec![]));
            let result = call_tool(state, &name, &args);
            resp_line(id, result)
        }
        "ping" => resp_line(id, Json::obj(vec![])),
        other => {
            if is_notification {
                return;
            }
            err_line(id, -32601, &format!("метод не найден: {}", other))
        }
    };
    let _ = resp.send(line);
}

/// Игровой цикл MCP-режима: между «кадрами» разбирает очередь запросов.
/// Единственный владелец интерпретатора (Lisp-состояния) — эта нить.
/// Когда reader'ы закрыли канал (EOF stdio) — цикл завершается.
pub fn mcp_engine_loop(
    state: &mut McpState,
    req_rx: Receiver<McpRequest>,
    resp_tx: Sender<String>,
) {
    for req in req_rx {
        handle_request(state, req, &resp_tx);
    }
}

pub fn run_mcp(script: &std::path::Path, fps: f64) -> Result<(), String> {
    crate::interp::set_mcp_quiet(true);
    let halrc = Rc::new(RefCell::new(crate::hal::Hal::new(
        crate::hal::OutputMode::Silent,
        320,
        240,
    )));
    let root = script
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let (ctx, cell) = crate::make_context(halrc.clone(), root);
    load_file(script, &ctx)?;
    let h = EngineHandle { ctx, cell, halrc };
    let mut state = McpState {
        h,
        frames: 0,
        fps: 0.0,
        dt: 1.0 / fps.max(1.0),
    };

    eprintln!(
        "[mcp] rlisp-engine MCP готов; инструментов: {} (stdio, JSON-RPC 2.0, reader/writer в фоне)",
        tools_list().len()
    );

    // каналы: запросы reader → игровая нить; ответы игровая нить → writer
    let (req_tx, req_rx) = std::sync::mpsc::channel::<McpRequest>();
    let (resp_tx, resp_rx) = std::sync::mpsc::channel::<String>();

    // writer: единственный писатель stdout — строки не перемешиваются
    let writer = std::thread::spawn(move || {
        let stdout = std::io::stdout();
        for line in resp_rx {
            let mut o = stdout.lock();
            let _ = writeln!(o, "{}", line);
            let _ = o.flush();
        }
    });

    // reader: разбирает строки stdin → McpRequest в канал; parse-ошибки
    // отвечаются сразу (данные протокола, интерпретатор не нужен)
    let rtx = resp_tx.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            match crate::json::parse(&line) {
                Ok(msg) => {
                    let method = msg
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let id = msg.get("id").cloned().unwrap_or(Json::Null);
                    let is_notification = matches!(msg.get("id"), None | Some(Json::Null));
                    let params = msg.get("params").cloned().unwrap_or(Json::Null);
                    if req_tx
                        .send(McpRequest {
                            id,
                            method,
                            params,
                            is_notification,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    if rtx
                        .send(err_line(Json::Null, -32700, &format!("parse error: {}", e)))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        // EOF stdio: канал запросов закрывается — игровой цикл завершается
    });

    mcp_engine_loop(&mut state, req_rx, resp_tx.clone());

    // игровая нить закончилась: закрываем ответы → writer дописывает и выходит
    drop(resp_tx);
    let _ = writer.join();
    eprintln!("[mcp] сеанс завершён, кадров выполнено: {}", state.frames);
    Ok(())
}

// ——————————————————————————— тесты этапа 2 ———————————————————————————

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EngineHandle;
    use std::rc::Rc;

    /// Игровой код загружается/шагает с рекурсией eval; в тестовом потоке
    /// стек по умолчанию 2 МБ (у главной нити — 8 МБ) — даём большой стек.
    fn with_big_stack(f: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(f)
            .expect("тестовый поток")
            .join()
            .expect("тест не паниковал");
    }

    fn state_with_game() -> McpState {
        let halrc = Rc::new(RefCell::new(crate::hal::Hal::new(
            crate::hal::OutputMode::Silent,
            320,
            240,
        )));
        let (ctx, cell) =
            crate::make_context(halrc.clone(), std::path::PathBuf::from("assets/lisp"));
        load_file(std::path::Path::new("assets/lisp/main3d.lisp"), &ctx)
            .expect("игра 3D загружается");
        McpState {
            h: EngineHandle { ctx, cell, halrc },
            frames: 0,
            fps: 0.0,
            dt: 1.0 / 30.0,
        }
    }

    /// Критерий этапа 2 «MCP hammer»: ≥100 запросов (spawn/entities/eval/
    /// step/screenshot) приходят от фонового потока, пока игровой цикл
    /// работает — ни паники, ни краха, на каждый запрос пришёл ответ.
    #[test]
    fn mcp_hammer_120_запросов_без_падения() {
        with_big_stack(|| {
            crate::interp::set_mcp_quiet(true);
            let mut state = state_with_game();
            let (req_tx, req_rx) = std::sync::mpsc::channel::<McpRequest>();
            let (resp_tx, resp_rx) = std::sync::mpsc::channel::<String>();

            // «ИИ-агент»: сыплет запросы из фонового потока (модель stdio-клиента)
            let n: i64 = 120;
            let agent = std::thread::spawn(move || {
                for i in 0..n {
                    let (tool, args) = match i % 6 {
                        0 => ("status", Json::obj(vec![])),
                        1 => ("lisp_eval", Json::obj(vec![("code", Json::str("(+ 1 2)"))])),
                        2 => (
                            "entity_spawn",
                            Json::obj(vec![
                                ("kind", Json::str("grunt")),
                                ("x", Json::num(1.5)),
                                ("z", Json::num(2.5)),
                            ]),
                        ),
                        3 => ("step", Json::obj(vec![("frames", Json::int(2))])),
                        4 => ("screenshot", Json::obj(vec![])),
                        _ => ("scene_get", Json::obj(vec![])),
                    };
                    // настоящий конверт JSON-RPC: tools/call { name, arguments }
                    let params = Json::obj(vec![("name", Json::str(tool)), ("arguments", args)]);
                    req_tx
                        .send(McpRequest {
                            id: Json::int(i),
                            method: "tools/call".into(),
                            params,
                            is_notification: false,
                        })
                        .unwrap();
                }
                // конец потока = EOF: канал закрывается, игровой цикл завершится
            });

            mcp_engine_loop(&mut state, req_rx, resp_tx);
            agent.join().expect("агент не паниковал");

            // на КАЖДЫЙ запрос пришёл ровно один ответ; ответы — валидный JSON
            let mut answered = 0;
            let mut errors = 0;
            for line in resp_rx {
                answered += 1;
                let m = crate::json::parse(&line).expect("ответ — не JSON");
                assert!(
                    m.get("result").is_some() || m.get("error").is_some(),
                    "ответ без result/error: {}",
                    line
                );
                if m.get("error").is_some() {
                    errors += 1;
                }
            }
            assert_eq!(answered, n, "не на все запросы пришли ответы");
            assert_eq!(errors, 0, "есть ошибки уровня протокола");
            assert!(state.frames > 0, "step-инструмент не двинул симуляцию");
        });
    }

    /// Совместимость протокола: initialize/tools/list/ping отвечают как раньше.
    #[test]
    fn mcp_initialize_tools_ping() {
        with_big_stack(|| {
            crate::interp::set_mcp_quiet(true);
            let mut state = state_with_game();
            let (req_tx, req_rx) = std::sync::mpsc::channel::<McpRequest>();
            let (resp_tx, resp_rx) = std::sync::mpsc::channel::<String>();
            for (method, id) in [("initialize", 1), ("tools/list", 2), ("ping", 3)] {
                req_tx
                    .send(McpRequest {
                        id: Json::int(id),
                        method: method.into(),
                        params: Json::obj(vec![]),
                        is_notification: false,
                    })
                    .unwrap();
            }
            drop(req_tx);
            mcp_engine_loop(&mut state, req_rx, resp_tx);
            let lines: Vec<String> = resp_rx.iter().collect();
            assert_eq!(lines.len(), 3);
            let init = crate::json::parse(&lines[0]).unwrap();
            assert_eq!(
                init.get("result")
                    .and_then(|r| r.get("protocolVersion"))
                    .and_then(|v| v.as_str()),
                Some("2024-11-05")
            );
            let tools = crate::json::parse(&lines[1]).unwrap();
            assert_eq!(
                tools
                    .get("result")
                    .and_then(|r| r.get("tools"))
                    .map(|t| matches!(t, Json::Arr(xs) if xs.len() == 10)),
                Some(true)
            );
        });
    }
}
