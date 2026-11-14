// src/runner.rs — основной цикл движка, eval выражений, hot reload и обработка ввода.

use crate::cli::Args;
use crate::hal::{Hal, OutputMode};
use crate::interp::{
    apply, env_define, env_get, env_new, fmt_value, load_file, parse_program, CoreCtx, Value,
};
use crate::{editor, gc, gpu, make_context, register_all, simd};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

pub fn run_eval(code: &str) -> Result<(), String> {
    let halrc = Rc::new(RefCell::new(Hal::new(OutputMode::Silent, 8, 8)));
    let (ctx, cell) = make_context(halrc, PathBuf::from("."));
    let prog = parse_program(code)?;
    let mut last = Value::Nil;
    // последний результат переживает сборки на границах форм — держим pin
    let pin = gc::pin(vec![last.clone()]);
    let g = *cell.borrow();
    for e in prog {
        last = crate::interp::eval(&e, g, 0)?;
        gc::pin_replace(&pin, vec![last.clone()]);
        gc::maybe_collect(&[g], &[]);
    }
    println!("{}", fmt_value(&last));
    drop(pin);
    let _ = ctx;
    Ok(())
}

fn parse_input_file(path: &str) -> Result<Vec<(f64, Value)>, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("input {}: {}", path, e))?;
    let mut out = vec![];
    for (ln, line) in src.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(format!(
                "input {}: строка {}: ожидается «ВРЕМЯ КЛАВИША down|up»",
                path,
                ln + 1
            ));
        }
        let t: f64 = parts[0]
            .parse()
            .map_err(|_| format!("input {}: строка {}: плохое время", path, ln + 1))?;
        let ev = if parts[2] == "down" {
            "key-down"
        } else {
            "key-up"
        };
        out.push((
            t,
            Value::List(vec![
                Value::Sym(ev.into()),
                Value::Str(parts[1].to_string()),
            ]),
        ));
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

fn spawn_stdin() -> std::sync::mpsc::Receiver<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 64];
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        loop {
            match lock.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if tx.send(b).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

pub fn snapshot_mtimes(files: &[PathBuf]) -> HashMap<PathBuf, SystemTime> {
    let mut m = HashMap::new();
    for p in files {
        if let Ok(mt) = std::fs::metadata(p).and_then(|md| md.modified()) {
            m.insert(p.clone(), mt);
        }
    }
    m
}

/// Hot reload по ТЗ §8: новое Lisp-окружение, перечитанные файлы,
/// состояние из defpersistent переносится из старого мира в новый.
pub fn reload_all(
    ctx: &CoreCtx,
    halrc: &Rc<RefCell<Hal>>,
    mtimes: &mut HashMap<PathBuf, SystemTime>,
) -> Result<usize, String> {
    let g_old = *ctx.global.borrow();
    let mut saved: Vec<(String, Value)> = vec![];
    if let Some(Value::Table(t)) = env_get(g_old, "*persistent*") {
        let keys: Vec<String> = t.borrow().keys().cloned().collect();
        for k in keys {
            if let Some(v) = env_get(g_old, &k) {
                saved.push((k, v));
            }
        }
    }
    let n_saved = saved.len();
    // persistent-значения переживают сборки во время load_file — pin-корни
    let _pin = gc::pin(saved.iter().map(|(_, v)| v.clone()).collect());
    // Этап 1 (менеджер GPU-ресурсов): старые текстуры, на которые не ссылается
    // persistent-состояние, освобождаются при reload — раньше они оставались в
    // реестре до рестарта (утечка ~64 КБ/текстуру за reload).
    let mut keep: Vec<i64> = vec![];
    for (_, v) in &saved {
        gpu::collect_int_refs(v, &mut keep, 0);
    }
    let invalidated = halrc.borrow_mut().invalidate_textures_except(&keep);
    if invalidated > 0 {
        eprintln!(
            "[hot-reload] GPU-ресурсы: освобождено текстур: {}",
            invalidated
        );
    }
    let new_env = env_new(None);
    *ctx.global.borrow_mut() = new_env;
    register_all(new_env, ctx, halrc);
    env_define(
        new_env,
        "*root*",
        Value::Str(ctx.root.to_string_lossy().into_owned()),
    );
    let files: Vec<PathBuf> = ctx.registry.borrow().clone();
    for p in &files {
        load_file(p, ctx)?;
    }
    for (k, v) in saved {
        if env_get(new_env, &k).is_some() {
            env_define(new_env, &k, v);
        }
    }
    // граница reload: старый мир (окружение/замыкания) собирается здесь
    gc::maybe_collect(&[*ctx.global.borrow()], &[]);
    *mtimes = snapshot_mtimes(&files);
    Ok(n_saved)
}

pub fn run_engine(args: &Args) -> Result<(), String> {
    let script = PathBuf::from(&args.script);
    let root = script
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let out_mode = if let Some(dir) = &args.capture {
        std::fs::create_dir_all(dir).map_err(|e| format!("capture {}: {}", dir, e))?;
        OutputMode::Png {
            dir: PathBuf::from(dir),
            every: args.every.max(1),
        }
    } else if args.ascii {
        OutputMode::Ascii
    } else {
        OutputMode::Silent
    };

    // — бэкенд рендера: --wgpu → wgpu за фасадом HAL; без адаптера/фичи —
    //   честный откат на программный растр (все скрипты работают как раньше) —
    let (backend, render_note) = if args.wgpu {
        let probe = gpu::create_wgpu_backend(args.width, args.height);
        if let Some(w) = gpu::fallback_warning(&probe) {
            eprintln!("{}", w);
        }
        match probe {
            Ok(b) => {
                let info = b.adapter_info();
                (gpu::Backend::Wgpu(b), format!("wgpu: {}", info))
            }
            Err(_) => (
                gpu::Backend::Software,
                "программный растр (fallback из --wgpu)".into(),
            ),
        }
    } else {
        (gpu::Backend::Software, "программный растр".into())
    };
    let halrc = Rc::new(RefCell::new(Hal::with_backend(
        out_mode,
        args.width,
        args.height,
        backend,
    )));
    let (ctx, cell) = make_context(halrc.clone(), root.clone());
    env_define(
        *cell.borrow(),
        "*root*",
        Value::Str(root.to_string_lossy().into_owned()),
    );

    // ——— детерминизм: --replay грузит запись и фиксирует seed ———
    // Seed обязан быть выставлен ДО загрузки сцены: иначе случайность,
    // потраченная в init сцены (первая цель, первый враг), не воспроизводится —
    // состояние ГПСЧ сбрасывалось уже после того, как init сдвинул его вперёд.
    if let Some(p) = &args.replay {
        let seed = crate::replay::start_replay(p)?;
        crate::interp::rng::set_seed(seed);
        println!(
            "replay: {} (seed {}) — кадры сверяются по hash-цепочке",
            p, seed
        );
    }

    load_file(&script, &ctx)?;
    let (w, h) = {
        let hb = halrc.borrow();
        (hb.w, hb.h)
    };

    // --width/--height — авторитет CLI: window-create сцены мог задать свой
    // размер (константы graphics.lisp) — возвращаем размер из аргументов и
    // синхронизируем *screen-w*/*screen-h*, чтобы HUD/небо сцены строились
    // под реальный фреймбуфер (раньше --width менял только буфер HAL).
    if (w, h) != (args.width, args.height) {
        halrc.borrow_mut().resize_window(args.width, args.height);
    }
    if args.width != 320 || args.height != 240 {
        let g = *cell.borrow();
        for (name, val) in [("*screen-w*", args.width), ("*screen-h*", args.height)] {
            let code = format!("(set! {} {})", name, val);
            for form in parse_program(&code)? {
                crate::interp::eval(&form, g, 0)?;
            }
        }
    }

    // v0.3: мост редактора (--serve) — публикуем кадр/сцену, принимаем команды
    let bridge = if args.serve {
        let b = editor::Bridge::new();
        b.set_audio(halrc.borrow().audio.clone());
        editor::install_bridge(b.clone());
        editor::start_server(b.clone(), args.port, root.clone())
            .map_err(|e| format!("редактор: порт {} занят: {}", args.port, e))?;
        println!(
            "редактор: http://127.0.0.1:{} (иерархия · инспектор · REPL · ассеты)",
            args.port
        );
        let assets: Vec<String> = ctx
            .registry
            .borrow()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        *b.assets.lock().unwrap() = assets;
        Some(b)
    } else {
        None
    };

    let events = match &args.input {
        Some(p) => parse_input_file(p)?,
        None => vec![],
    };

    // ——— детерминизм: --record пишет свою запись ———
    if let Some(p) = &args.record {
        let seed = crate::interp::rng::initial_seed();
        crate::replay::start_record(p.clone(), seed, args.fps);
        println!("record: {} (seed {}) — по завершении файл прогона", p, seed);
    }
    if crate::replay::active() && !(args.fast || args.input.is_some() || args.capture.is_some()) {
        println!("replay: включён фиксированный dt (детерминированный режим)");
    }
    let files: Vec<PathBuf> = ctx.registry.borrow().clone();
    let mut mtimes = snapshot_mtimes(&files);
    let n_files = files.len();

    println!(
        "rlisp-engine v{} | SIMD: {} | рендер: {} | экран {}x{} | lisp-файлов: {} | скрипт: {}",
        env!("CARGO_PKG_VERSION"),
        simd::level_name(simd::detect()),
        render_note,
        w,
        h,
        n_files,
        args.script
    );
    if args.ascii {
        println!("управление: wasd — движение, q — выход");
        print!("\x1b[2J");
        std::io::stdout().flush().ok();
    }

    let rx = if args.stdin_keys {
        let _ = Command::new("stty").arg("raw").arg("-echo").status();
        Some(spawn_stdin())
    } else {
        None
    };

    let fixed_dt = 1.0 / args.fps;
    let use_fixed =
        args.fast || args.capture.is_some() || args.input.is_some() || crate::replay::active();
    let mut sim_time = 0f64;
    let mut ev_i = 0usize;
    let mut stdin_held: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut frames: u64 = 0;
    let mut reloads: u64 = 0;
    let mut quit = false;
    let wall_start = Instant::now();
    let mut last_frame_at = Instant::now();
    let mut next_frame = wall_start + Duration::from_secs_f64(fixed_dt);
    // профилировка кадра: RLISP_PROFILE=1 → разбивка update/draw/frame-end/
    // gc/прочее (мс на кадр, среднее за 120 кадров)
    let prof = std::env::var_os("RLISP_PROFILE").is_some();
    let mut t_upd = Duration::ZERO;
    let mut t_draw = Duration::ZERO;
    let mut t_fe = Duration::ZERO;
    let mut t_gc = Duration::ZERO;
    let mut t_all = Duration::ZERO;
    let mut t_frame = Instant::now();

    while frames < args.frames {
        // ввод: при воспроизведении — из записи (кадр в кадр), иначе из сценария
        let mut frame_inputs: Vec<String> = Vec::new();
        if let Some(recorded) = crate::replay::frame_inputs(frames) {
            for v in recorded {
                if let Some(enc) = crate::replay::encode_input(&v) {
                    frame_inputs.push(enc);
                }
                halrc.borrow_mut().queue.push(v);
            }
        } else {
            while ev_i < events.len() && events[ev_i].0 <= sim_time {
                let v = events[ev_i].1.clone();
                if let Some(enc) = crate::replay::encode_input(&v) {
                    frame_inputs.push(enc);
                }
                halrc.borrow_mut().queue.push(v);
                ev_i += 1;
            }
        }
        // авто-release для stdin-нажатий (key-up там физически не приходит)
        if !stdin_held.is_empty() {
            let due: Vec<String> = stdin_held
                .iter()
                .filter(|(_, until)| **until <= frames)
                .map(|(k, _)| k.clone())
                .collect();
            for k in due {
                stdin_held.remove(&k);
                let ev = Value::List(vec![Value::Sym("key-up".into()), Value::Str(k)]);
                if let Some(enc) = crate::replay::encode_input(&ev) {
                    frame_inputs.push(enc);
                }
                halrc.borrow_mut().queue.push(ev);
            }
        }
        // ввод с клавиатуры
        if let Some(rx) = &rx {
            while let Ok(b) = rx.try_recv() {
                match b {
                    b'q' | b'Q' | 3 => quit = true,
                    b'a'..=b'z' => {
                        let k = (b as char).to_string();
                        let ev =
                            Value::List(vec![Value::Sym("key-down".into()), Value::Str(k.clone())]);
                        if let Some(enc) = crate::replay::encode_input(&ev) {
                            frame_inputs.push(enc);
                        }
                        halrc.borrow_mut().queue.push(ev);
                        // stdin не отдаёт key-up: авто-release через 12 sim-кадров
                        stdin_held.insert(k, frames + 12);
                    }
                    b'A'..=b'Z' => {
                        let k = (b as char).to_lowercase().to_string();
                        let ev = Value::List(vec![Value::Sym("key-up".into()), Value::Str(k)]);
                        if let Some(enc) = crate::replay::encode_input(&ev) {
                            frame_inputs.push(enc);
                        }
                        halrc.borrow_mut().queue.push(ev);
                    }
                    _ => {}
                }
            }
        }
        if quit {
            break;
        }

        // v0.3: команды редактора (eval/keys) + ручной reload
        let mut editor_skip = false;
        if let Some(b) = &bridge {
            if editor::drain_editor_cmds(b, &cell, &halrc) {
                match reload_all(&ctx, &halrc, &mut mtimes) {
                    Ok(n) => {
                        reloads += 1;
                        b.log_line(format!("[reload] {} persistent-значений восстановлено", n));
                    }
                    Err(e) => b.log_line(format!("[reload] ошибка: {}", e)),
                }
                let assets: Vec<String> = ctx
                    .registry
                    .borrow()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                *b.assets.lock().unwrap() = assets;
            }
            let paused = b.paused.load(std::sync::atomic::Ordering::Relaxed);
            let step = b.step_req.swap(false, std::sync::atomic::Ordering::Relaxed);
            editor_skip = paused && !step;
        }

        // hot reload: следим за .lisp-файлами каждый кадр
        let mut changed: Option<PathBuf> = None;
        for p in &files {
            if let Ok(mt) = std::fs::metadata(p).and_then(|md| md.modified()) {
                if mtimes.get(p) != Some(&mt) {
                    changed = Some(p.clone());
                    break;
                }
            }
        }
        if let Some(p) = changed {
            match reload_all(&ctx, &halrc, &mut mtimes) {
                Ok(n) => {
                    reloads += 1;
                    eprintln!(
                        "[hot-reload] кадр {}: {} перезагружен; окружение пересобрано, {} persistent-значений восстановлено",
                        frames, p.display(), n
                    );
                }
                Err(e) => eprintln!(
                    "[hot-reload] ошибка: {} — продолжаем на старом окружении",
                    e
                ),
            }
        }

        // dt
        let dt = if use_fixed {
            fixed_dt
        } else {
            let d = last_frame_at.elapsed().as_secs_f64().clamp(0.001, 0.05);
            last_frame_at = Instant::now();
            d
        };

        let need_readback = if let Some(b) = &bridge {
            frames <= 1
                || b.frame_requested
                    .swap(false, std::sync::atomic::Ordering::Relaxed)
        } else {
            false
        };
        halrc.borrow_mut().readback_requested = need_readback;

        // кадр: update(dt) → draw() → frame-end() (в паузе редактора — пропуск)
        // RLISP_PROFILE=1 — накопление разбивки времени кадра (update/draw/
        // frame-end/GC) и печать каждые 120 кадров + в итоге
        if !editor_skip {
            let g = *cell.borrow();
            let mut err: Option<String> = None;
            let t0 = Instant::now();
            if let Some(f) = env_get(g, "update") {
                if let Err(e) = apply(&f, &[Value::Float(dt)], 0) {
                    err = Some(format!("update: {}", e));
                }
            }
            if prof {
                t_upd += t0.elapsed();
            }
            let t0 = Instant::now();
            if err.is_none() {
                if let Some(f) = env_get(g, "draw") {
                    if let Err(e) = apply(&f, &[], 0) {
                        err = Some(format!("draw: {}", e));
                    }
                }
            }
            if prof {
                t_draw += t0.elapsed();
            }
            let t0 = Instant::now();
            if err.is_none() {
                if let Some(f) = env_get(g, "frame-end") {
                    let _ = apply(&f, &[], 0);
                }
            }
            if prof {
                t_fe += t0.elapsed();
            }
            if let Some(e) = err {
                return Err(format!("кадр {}: {}", frames, e));
            }
        }

        // Этап 2: граница форм — авто-сборка GC. Очередь ввода (Value-события)
        // передаём как дополнительные корни: в будущем события могут нести пары.
        let queue_roots = halrc.borrow().queue.clone();
        let t0 = Instant::now();
        gc::maybe_collect(&[*cell.borrow()], &queue_roots);
        if prof {
            t_gc += t0.elapsed();
        }

        if prof {
            t_all += t_frame.elapsed();
            t_frame = Instant::now();
            if frames % 120 == 119 {
                let k = 120.0 / (t_all.as_secs_f64() + 1e-9);
                eprintln!(
                    "[profile] update {:.1} | draw {:.1} | frame-end {:.1} | gc {:.1} | прочее {:.1} мс/кадр ({} fps)",
                    t_upd.as_secs_f64() * 1e3 / 120.0,
                    t_draw.as_secs_f64() * 1e3 / 120.0,
                    t_fe.as_secs_f64() * 1e3 / 120.0,
                    t_gc.as_secs_f64() * 1e3 / 120.0,
                    (t_all - t_upd - t_draw - t_fe - t_gc).as_secs_f64() * 1e3 / 120.0,
                    (k as u32)
                );
                t_all = Duration::ZERO;
                (t_upd, t_draw, t_fe, t_gc) = (
                    Duration::ZERO,
                    Duration::ZERO,
                    Duration::ZERO,
                    Duration::ZERO,
                );
            }
        }

        // детерминизм: hash кадра → цепочка (record — накопить, replay — сверить)
        if crate::replay::active() {
            let fbh = {
                let hal = halrc.borrow();
                crate::replay::fbv_hash(&hal.fb)
            };
            if let Err(e) = crate::replay::tick(frames, fbh, frame_inputs.clone()) {
                eprintln!("replay: {}", e);
                if crate::replay::mode() == Some(crate::replay::Mode::Replay) {
                    return Err(format!("воспроизведение разошлось — {}", e));
                }
            }
            if crate::replay::finished() {
                break; // запись пройдена до конца
            }
        }

        frames += 1;
        sim_time += dt;

        // v0.3: публикация кадра/сцены/статистики в мост редактора
        if let Some(b) = &bridge {
            {
                let mut hal = halrc.borrow_mut();
                if hal.readback_requested {
                    // Если (gpu-present) не вызывался из Lisp — досылаем с readback
                    let _ = hal.flush_gpu(need_readback);
                    hal.readback_requested = false;
                }
                if need_readback {
                    *b.fb.lock().unwrap() = hal.fb.clone();
                    *b.fb_wh.lock().unwrap() = (hal.w, hal.h);
                }
                b.drawcalls
                    .store(hal.r3.drawn, std::sync::atomic::Ordering::Relaxed);
            }
            b.frame.store(frames, std::sync::atomic::Ordering::Relaxed);
            // fps считаем только по реально отрисованным кадрам: в паузе
            // редактора цикл крутится вхолостую и «fps 12000» — ложь
            if !editor_skip {
                let inst = 1.0 / last_frame_at.elapsed().as_secs_f64().max(1e-6);
                let mut f = b.fps.lock().unwrap();
                *f = if *f <= 0.0 {
                    inst
                } else {
                    *f * 0.9 + inst * 0.1
                };
            }
            editor::publish_scene(&cell, b);
            editor::publish_extras(&cell, b);
        }

        // удержание реального времени (для demo hot reload по настенным часам)
        if !args.fast {
            next_frame += Duration::from_secs_f64(fixed_dt);
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else {
                next_frame = now;
            }
        }
    }

    if args.stdin_keys {
        let _ = Command::new("stty").arg("sane").status();
    }

    let g = *cell.borrow();
    let score = env_get(g, "*score*")
        .map(|v| fmt_value(&v))
        .unwrap_or_else(|| "?".into());
    let wall = wall_start.elapsed().as_secs_f64();
    println!(
        "итог: кадров {} за {:.1}с ({:.1} fps) | hot-reload: {} | score: {}",
        frames,
        wall,
        frames as f64 / wall.max(1e-9),
        reloads,
        score
    );
    if crate::replay::active() {
        let diverged = crate::replay::diverged();
        if let Some(report) = crate::replay::stop() {
            println!("{}", report);
        }
        if diverged {
            return Err("воспроизведение разошлось: прогон не совпал с записью".into());
        }
    }
    Ok(())
}
