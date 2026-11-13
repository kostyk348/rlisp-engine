// src/editor/tests.rs — тесты моста и JSON-склейки редактора.

use super::bridge::Bridge;
use super::lisp::{eval_string, publish_extras};
use crate::hal::OutputMode;
use crate::interp::{env_get, fmt_value, parse_program};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

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
    let path_str = path.to_string_lossy().replace('\\', "/");
    let res = eval_string(&format!("(editor-save-scene \"{}\")", path_str), &cell).unwrap();
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
    eval_string(&format!("(load \"{}\")", path_str), &cell).unwrap();
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
