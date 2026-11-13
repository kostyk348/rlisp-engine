// src/editor/routes.rs — маршруты HTTP API редактора.
// Один запрос = один матч по (метод, путь); каждый обработчик отвечает и
// возвращается. Всё, что меняет движок, кладётся в очередь команд моста —
// исполняет её игровая нить в начале кадра (гонок с интерпретатором нет).

use super::bridge::Bridge;
use super::http::{body_field, http_err, http_json, http_res, project_root, query, resolve_lisp};
use crate::json::Json;
use crate::png;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub(crate) fn serve(mut stream: TcpStream, bridge: Arc<Bridge>, root: std::path::PathBuf) {
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
            let html = include_str!("../../assets/editor/index.html");
            http_res(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                html.as_bytes().to_vec(),
            );
        }
        ("GET", "/frame.png") => {
            bridge.frame_requested.store(true, Ordering::Relaxed);
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
                ("replay", Json::Str(bridge.replay.lock().unwrap().clone())),
                ("camera", Json::Str(camera)),
                ("prefabs", Json::Str(prefabs)),
                // аудиосистема: список звуков, число голосов, мастер-громкость
                (
                    "audio",
                    if let Some(audio_arc) = bridge.audio.lock().unwrap().as_ref() {
                        let m = audio_arc.lock().unwrap();
                        let sounds_arr: Vec<Json> = m
                            .sounds
                            .values()
                            .map(|s| {
                                Json::obj(vec![
                                    ("id", Json::int(s.id as i64)),
                                    ("name", Json::str(&s.name)),
                                    ("duration", Json::num(s.duration_secs() as f64)),
                                    ("sample_rate", Json::int(s.sample_rate as i64)),
                                    ("channels", Json::int(s.channels as i64)),
                                ])
                            })
                            .collect();
                        Json::obj(vec![
                            ("sounds", Json::Arr(sounds_arr)),
                            ("voices", Json::int(m.voices.len() as i64)),
                            ("master_volume", Json::num(m.master_volume as f64)),
                        ])
                    } else {
                        Json::obj(vec![
                            ("sounds", Json::Arr(vec![])),
                            ("voices", Json::int(0)),
                            ("master_volume", Json::num(1.0)),
                        ])
                    },
                ),
            ]);
            http_json(&mut stream, &j);
        }
        ("GET", "/api/file") => {
            let p = q.get("path").unwrap_or("").to_string();
            match resolve_lisp(&root, &p) {
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
        ("POST", "/api/file-new") => match body_field(&body_str, "path") {
            Some(p) => match resolve_lisp(&root, &p) {
                Some(canon) => {
                    if canon.exists() {
                        http_err(&mut stream, "409", "файл уже есть");
                    } else {
                        let tpl = format!(
                                ";; {} — новый файл; hot reload подхватит при сохранении\n(editor-log \"создан {}\")\n",
                                p, p
                            );
                        match std::fs::write(&canon, tpl.as_bytes()) {
                            Ok(_) => {
                                bridge.log_line(format!("[new] {}", p));
                                http_json(&mut stream, &Json::obj(vec![("ok", Json::Bool(true))]));
                            }
                            Err(e) => http_err(&mut stream, "500", &format!("{}", e)),
                        }
                    }
                }
                None => http_err(&mut stream, "403", "только .lisp внутри assets/lisp"),
            },
            None => http_err(&mut stream, "400", "нужно {path}"),
        },
        ("POST", "/api/file-delete") => match body_field(&body_str, "path") {
            Some(p) => match resolve_lisp(&root, &p) {
                Some(canon) => match std::fs::remove_file(&canon) {
                    Ok(_) => {
                        bridge.log_line(format!("[del] {}", p));
                        http_json(&mut stream, &Json::obj(vec![("ok", Json::Bool(true))]));
                    }
                    Err(e) => http_err(&mut stream, "404", &format!("{}", e)),
                },
                None => http_err(&mut stream, "403", "только .lisp внутри assets/lisp"),
            },
            None => http_err(&mut stream, "400", "нужно {path}"),
        },
        // Разбор файла прогона (RLISP-REPLAY 1): что записано и с какого кадра.
        // Сам прогон не запускается — это делает CLI (`--replay`), здесь только
        // метаданные для панели детерминизма.
        ("POST", "/api/replay-inspect") => {
            match body_field(&body_str, "path") {
                Some(p) => {
                    // прогоны лежат в корне проекта (saves/…), а root редактора —
                    // это assets/lisp: пробуем корень проекта, затем сам root,
                    // абсолютный путь — как есть
                    let full = if std::path::Path::new(&p).is_absolute() {
                        std::path::PathBuf::from(&p)
                    } else {
                        let a = project_root(&root).join(&p);
                        if a.exists() {
                            a
                        } else {
                            root.join(&p)
                        }
                    };
                    match std::fs::read_to_string(&full) {
                        Ok(text) => match crate::replay::Session::parse(&text) {
                            Ok(sess) => http_json(
                                &mut stream,
                                &Json::obj(vec![
                                    ("ok", Json::Bool(true)),
                                    ("start", Json::int(sess.start_frame as i64)),
                                    ("frames", Json::int(sess.frames.len() as i64)),
                                    (
                                        "chain",
                                        Json::Str(format!(
                                            "{:016x}",
                                            sess.recorded_chain.unwrap_or(sess.chain)
                                        )),
                                    ),
                                    ("seed", Json::int(sess.seed as i64)),
                                    ("fps", Json::num(sess.fps)),
                                    ("path", Json::Str(p.clone())),
                                ]),
                            ),
                            Err(e) => http_json(
                                &mut stream,
                                &Json::obj(vec![
                                    ("ok", Json::Bool(false)),
                                    ("error", Json::Str(e.clone())),
                                ]),
                            ),
                        },
                        Err(e) => http_json(
                            &mut stream,
                            &Json::obj(vec![
                                ("ok", Json::Bool(false)),
                                ("error", Json::Str(format!("{}: {}", p, e))),
                            ]),
                        ),
                    }
                }
                None => http_err(&mut stream, "400", "нужно {path}"),
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
                    match resolve_lisp(&root, &p) {
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
        ("GET", "/api/audio-events") => {
            let since: u64 = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
            let events = if let Some(audio_arc) = bridge.audio.lock().unwrap().as_ref() {
                let m = audio_arc.lock().unwrap();
                let evs: Vec<Json> = m
                    .events
                    .iter()
                    .filter(|e| e.id > since)
                    .map(|e| {
                        Json::obj(vec![
                            ("id", Json::int(e.id as i64)),
                            ("sound_id", Json::int(e.sound_id as i64)),
                            ("name", Json::str(&e.name)),
                            ("volume", Json::num(e.volume as f64)),
                            ("pitch", Json::num(e.pitch as f64)),
                            ("pan", Json::num(e.pan as f64)),
                            ("action", Json::str(&e.action)),
                        ])
                    })
                    .collect();
                evs
            } else {
                vec![]
            };
            http_json(&mut stream, &Json::Arr(events));
        }
        ("GET", "/api/sound-wav") => {
            let id: u64 = q.get("id").and_then(|s| s.parse().ok()).unwrap_or(0);
            let wav_bytes = if let Some(audio_arc) = bridge.audio.lock().unwrap().as_ref() {
                let m = audio_arc.lock().unwrap();
                m.sounds.get(&id).map(crate::audio::WavCodec::encode)
            } else {
                None
            };
            match wav_bytes {
                Some(bytes) => http_res(&mut stream, "200 OK", "audio/wav", bytes),
                None => http_err(&mut stream, "404", "звук не найден"),
            }
        }
        _ => http_err(&mut stream, "404", "нет такого маршрута"),
    }
}
