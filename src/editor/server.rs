// src/editor/server.rs — жизненный цикл HTTP-сервера редактора.
// Один поток на соединение (редактор — это localhost-инструмент, не сервис),
// мост — единственная общая точка с игровой нитью.

use super::bridge::Bridge;
use super::routes::serve;
use std::net::TcpListener;
use std::sync::Arc;

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

pub(crate) fn bridge() -> Option<&'static Arc<Bridge>> {
    BRIDGE.get()
}
