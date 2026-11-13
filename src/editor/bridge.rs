// src/editor/bridge.rs — мост игровой нити и HTTP-нити редактора.
// Только разделяемое состояние: кадр, сцена, команды, результаты, лог.
// Никакой семантики сцены здесь нет — она в assets/lisp/editor.lisp.

use std::collections::VecDeque;
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
    /// Статус детерминизма (src/replay.rs): mode/frames/chain — панель редактора.
    pub replay: Mutex<String>,
    /// Этап 6: камера игры — JSON от хука (editor-camera), «null» если хука нет.
    pub camera: Mutex<String>,
    /// Этап 6: префабы игры — JSON от хука (prefab-list), «null» если хука нет.
    pub prefabs: Mutex<String>,
    /// Аудиосистема: доступ к микшеру и событиям для браузера
    pub audio: Mutex<Option<Arc<Mutex<crate::audio::AudioMixer>>>>,
    /// Флаг запроса кадра браузером (для on-demand GPU readback)
    pub frame_requested: AtomicBool,
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
            replay: Mutex::new(String::new()),
            camera: Mutex::new("null".into()),
            prefabs: Mutex::new("null".into()),
            audio: Mutex::new(None),
            frame_requested: AtomicBool::new(true),
        })
    }

    pub fn set_audio(&self, audio: Arc<Mutex<crate::audio::AudioMixer>>) {
        *self.audio.lock().unwrap() = Some(audio);
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
