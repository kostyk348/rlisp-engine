// src/editor/mod.rs — редактор аля Unity поверх движка (--serve).
//
// Раскладка (правило §1 AI_AGENTS.md — ни один файл не больше 800 строк):
//   bridge.rs      — разделяемое состояние игровой и HTTP-нитей;
//   http.rs        — HTTP/1.1, query, тело, защита путей;
//   routes.rs      — маршруты API (кадр, сцена, eval, файлы, прогоны);
//   server.rs      — старт сервера и поток на соединение;
//   lisp.rs        — хуки editor-*, REPL, публикация сцены, разбор команд;
//   diagnostics.rs — ошибки Lisp с номером строки для консоли;
//   tests.rs       — тесты.
//
// Философия прежняя: сервер — «железо», вся семантика сцены — Lisp
// (assets/lisp/editor.lisp), UI — assets/editor/index.html.

pub mod bridge;
pub mod diagnostics;
pub mod http;
pub mod lisp;
pub mod routes;
pub mod server;

#[cfg(test)]
mod tests;

pub use bridge::Bridge;
pub use lisp::{
    drain_editor_cmds, eval_string, publish_extras, publish_scene, register_editor, step_frame,
    EngineHandle,
};
pub use server::{install_bridge, start_server};
