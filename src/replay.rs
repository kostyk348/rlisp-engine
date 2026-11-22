// src/replay.rs — детерминированное воспроизведение прогона.
//
// Идея та же, что и во всём движке: Rust — «железо». Он умеет только снять
// байты кадра и посчитать по ним FNV-1a; всё, что происходит в кадре, —
// Lisp-сцена. Поэтому «воспроизводимость» здесь не магия и не сериализация
// мира: фиксируется (seed ГПСЧ + поток ввода по кадрам + hash каждого кадра),
// а проверка — побитовая: hash-цепочка обязана совпасть кадр в кадр.
//
// Формат файла — текстовый, диффабельный, ноль зависимостей:
//
//   RLISP-REPLAY 1
//   seed 1234567890123456789
//   fps 60
//   frames 3
//   0 9f2c1d3e4f5a6b7c key-down:w key-up:w
//   1 1a3b5c7d9e0f1122
//   2 77aa66bb55cc44dd key-down:a
//   chain 0f1e2d3c4b5a6978
//
// Нативы (см. interp/builtins_replay.rs) отдают это же состояние в Lisp:
// (replay-record! "saves/run.rlrp"), (replay-stop!), (replay-load! …),
// (replay-status) — редактор дергает их через /api/eval и показывает панель.

use crate::interp::value::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAGIC: &str = "RLISP-REPLAY 1";

const MIX_A: u64 = 0x9E37_79B9_7F4A_7C15;
const MIX_B: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// FNV-1a 64 по байтам кадра (fb = RGBA, 4 байта на пиксель).
pub fn fbv_hash(fb: &[u8]) -> u64 {
    fnv1a(fb)
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Шаг цепочки: chain_{n+1} = mix(chain_n, frame, hash). Любое расхождение
/// кадра ломает все последующие хеши — как в hash-chain памяти.
pub fn chain_step(chain: u64, frame: u64, hash: u64) -> u64 {
    let mut x =
        chain ^ hash.wrapping_mul(MIX_B) ^ frame.wrapping_mul(MIX_A) ^ 0x5bf0_3635_1a2b_3c4d;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    x
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Record,
    Replay,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Record => "record",
            Mode::Replay => "replay",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub hash: u64,
    /// События ввода этого кадра в виде "key-down:w" (порядок сохраняется).
    pub inputs: Vec<String>,
}

pub struct Session {
    pub mode: Mode,
    pub path: PathBuf,
    pub seed: u64,
    pub fps: f64,
    pub expected_frames: u64,
    /// Кадр, с которого начата запись: живая запись из редактора стартует не с нуля.
    pub start_frame: u64,
    pub chain: u64,
    /// Chain из файла записи (только у воспроизведения) — сверяется в конце.
    pub recorded_chain: Option<u64>,
    pub frames: Vec<Frame>,
    pub cursor: usize,
    pub mismatches: Vec<String>,
    /// Прогон прошёл запись до конца.
    pub done: bool,
    /// Сколько кадров реально сверено (в воспроизведении).
    pub verified: u64,
}

impl Session {
    pub fn new_record(path: impl Into<PathBuf>, seed: u64, fps: f64) -> Self {
        Session {
            mode: Mode::Record,
            path: path.into(),
            seed,
            fps,
            expected_frames: 0,
            start_frame: 0,
            chain: 0,
            recorded_chain: None,
            frames: Vec::new(),
            cursor: 0,
            mismatches: Vec::new(),
            done: false,
            verified: 0,
        }
    }

    /// Шаг кадра: в записи — накопить, в воспроизведении — сверить.
    pub fn step(&mut self, frame: u64, hash: u64, inputs: Vec<String>) -> Result<(), String> {
        if self.frames.is_empty() && frame != self.start_frame {
            // запись начинается с произвольного кадра (живая запись из редактора)
            self.start_frame = frame;
        }
        if frame < self.start_frame {
            return Ok(()); // кадры до начала записи не сверяются и в цепочку не входят
        }
        let idx = (frame - self.start_frame) as usize;
        if self.mode == Mode::Replay && idx >= self.frames.len() {
            // запись кончилась — сверка завершена, лишний кадр в цепочку не идёт
            self.done = true;
            return Ok(());
        }
        self.chain = chain_step(self.chain, frame, hash);
        match self.mode {
            Mode::Record => {
                self.frames.push(Frame { hash, inputs });
                Ok(())
            }
            Mode::Replay => {
                let want = &self.frames[idx];
                self.verified += 1;
                let mut bad: Vec<String> = Vec::new();
                if want.hash != hash {
                    bad.push(format!("hash кадра {:016x} ≠ {:016x}", hash, want.hash));
                }
                if want.inputs != inputs {
                    bad.push(format!(
                        "ввод {:?} ≠ {:?}",
                        inputs.join(" "),
                        want.inputs.join(" ")
                    ));
                }
                if bad.is_empty() {
                    Ok(())
                } else {
                    let msg = format!("кадр {}: {}", frame, bad.join("; "));
                    self.mismatches.push(msg.clone());
                    Err(msg)
                }
            }
        }
    }

    /// Ввод, который нужно подать в этом кадре (только в воспроизведении).
    pub fn inputs_for(&self, frame: u64) -> Option<Vec<Value>> {
        if self.mode != Mode::Replay {
            return None;
        }
        if frame < self.start_frame {
            return None;
        }
        self.frames
            .get((frame - self.start_frame) as usize)
            .map(|f| f.inputs.iter().filter_map(|s| decode_input(s)).collect())
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(MAGIC);
        out.push('\n');
        out.push_str(&format!("seed {}\n", self.seed));
        out.push_str(&format!("fps {}\n", self.fps));
        out.push_str(&format!("frames {}\n", self.frames.len()));
        out.push_str(&format!("start {}\n", self.start_frame));
        for (i, f) in self.frames.iter().enumerate() {
            out.push_str(&format!("{} {:016x}", i, f.hash));
            for inp in &f.inputs {
                out.push(' ');
                out.push_str(inp);
            }
            out.push('\n');
        }
        out.push_str(&format!("chain {:016x}\n", self.chain));
        out
    }

    pub fn parse(text: &str) -> Result<Session, String> {
        let mut lines = text.lines();
        let head = lines.next().unwrap_or("").trim();
        if head != MAGIC {
            return Err(format!("не RLISP-REPLAY файл: «{}»", head));
        }
        let mut seed = 0u64;
        let mut fps = 60.0f64;
        let mut expected = 0u64;
        let mut start = 0u64;
        let mut chain = 0u64;
        let mut frames: Vec<Frame> = Vec::new();
        for (ln, line) in lines.enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split_whitespace();
            let key = it.next().unwrap();
            match key {
                "seed" => {
                    seed = it
                        .next()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| format!("строка {}: плохой seed", ln + 2))?
                }
                "fps" => fps = it.next().and_then(|s| s.parse().ok()).unwrap_or(60.0),
                "frames" => expected = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
                "start" => start = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
                "chain" => {
                    chain = it
                        .next()
                        .and_then(|s| u64::from_str_radix(s, 16).ok())
                        .unwrap_or(0)
                }
                idx => {
                    let i: usize = idx
                        .parse()
                        .map_err(|_| format!("строка {}: плохой номер кадра «{}»", ln + 2, idx))?;
                    if i != frames.len() {
                        return Err(format!(
                            "строка {}: кадр {} не по порядку (ожидался {})",
                            ln + 2,
                            i,
                            frames.len()
                        ));
                    }
                    let hex = it
                        .next()
                        .ok_or_else(|| format!("строка {}: нет hash кадра", ln + 2))?;
                    let hash = u64::from_str_radix(hex, 16)
                        .map_err(|_| format!("строка {}: плохой hash «{}»", ln + 2, hex))?;
                    let inputs: Vec<String> = it.map(|s| s.to_string()).collect();
                    frames.push(Frame { hash, inputs });
                }
            }
        }
        if expected != frames.len() as u64 {
            return Err(format!(
                "заголовок обещает {} кадров, в файле {}",
                expected,
                frames.len()
            ));
        }
        Ok(Session {
            mode: Mode::Replay,
            path: PathBuf::new(),
            seed,
            fps,
            expected_frames: expected,
            start_frame: start,
            chain: 0,
            recorded_chain: Some(chain),
            frames,
            cursor: 0,
            mismatches: Vec::new(),
            done: false,
            verified: 0,
        })
    }

    pub fn summary(&self) -> String {
        let mut problems = self.mismatches.len();
        let mut chain_note = String::new();
        if let Some(want) = self.recorded_chain {
            if want != self.chain {
                problems += 1;
                chain_note = format!(" (в записи {:016x})", want);
            }
        }
        let verdict = if problems == 0 {
            "OK".to_string()
        } else {
            format!("РАСХОЖДЕНИЕ ×{}", problems)
        };
        let shown = match self.mode {
            Mode::Record => format!("{} кадров", self.frames.len()),
            Mode::Replay => format!("{} из {} кадров сверено", self.verified, self.frames.len()),
        };
        format!(
            "replay {}: {}, chain {:016x}{}, {}",
            self.mode.name(),
            shown,
            self.chain,
            chain_note,
            verdict
        )
    }
}

/// Кодирование события ввода: (key-down "w") → "key-down:w".
pub fn encode_input(v: &Value) -> Option<String> {
    if let Value::List(items) = v {
        if items.len() == 2 {
            let kind = match &items[0] {
                Value::Sym(s) => s.to_string(),
                Value::Str(s) => s.clone(),
                _ => return None,
            };
            let key = match &items[1] {
                Value::Str(s) => s.clone(),
                Value::Sym(s) => s.to_string(),
                _ => return None,
            };
            return Some(format!("{}:{}", kind, key));
        }
    }
    None
}

/// Обратное преобразование: "key-down:w" → (key-down "w").
pub fn decode_input(s: &str) -> Option<Value> {
    let (kind, key) = s.split_once(':')?;
    Some(Value::List(vec![
        Value::Sym(kind.into()),
        Value::Str(key.to_string()),
    ]))
}

// ————————————————————— глобальное состояние сессии —————————————————————

static STATE: Mutex<Option<Session>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut Option<Session>) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

pub fn active() -> bool {
    with_state(|s| s.is_some())
}

pub fn mode() -> Option<Mode> {
    with_state(|s| s.as_ref().map(|x| x.mode))
}

/// Начать запись. Возвращает seed, который попал в файл.
pub fn start_record(path: impl Into<PathBuf>, seed: u64, fps: f64) -> u64 {
    with_state(|s| *s = Some(Session::new_record(path, seed, fps)));
    seed
}

/// Загрузить запись и включить воспроизведение. Возвращает seed из файла.
pub fn start_replay(path: &str) -> Result<u64, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("replay {}: {}", path, e))?;
    let mut session = Session::parse(&text)?;
    session.path = PathBuf::from(path);
    let seed = session.seed;
    with_state(|s| *s = Some(session));
    Ok(seed)
}

/// Остановить сессию: в записи — сохранить файл, в воспроизведении — вердикт.
pub fn stop() -> Option<String> {
    let session = with_state(|s| s.take())?;
    let mut session = session;
    session.done = true;
    if session.mode == Mode::Record {
        if let Some(dir) = session.path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).ok();
            }
        }
        match std::fs::write(&session.path, session.serialize()) {
            Ok(()) => Some(format!(
                "{} → {}",
                session.summary(),
                session.path.display()
            )),
            Err(e) => Some(format!(
                "запись не удалась ({}): {}",
                session.path.display(),
                e
            )),
        }
    } else {
        Some(session.summary())
    }
}

/// Шаг кадра из основного цикла.
pub fn tick(frame: u64, hash: u64, inputs: Vec<String>) -> Result<(), String> {
    with_state(|s| match s.as_mut() {
        Some(session) => session.step(frame, hash, inputs),
        None => Ok(()),
    })
}

/// Вход для кадра (Some только в воспроизведении).
pub fn frame_inputs(frame: u64) -> Option<Vec<Value>> {
    with_state(|s| s.as_ref().and_then(|session| session.inputs_for(frame)))
}

/// Состояние для Lisp/редактора: таблица со строками и числами.
pub fn status_table() -> Value {
    use std::collections::HashMap;
    let t: HashMap<String, Value> = with_state(|s| {
        let mut t = HashMap::new();
        match s.as_ref() {
            None => {
                t.insert("mode".into(), Value::Sym("none".into()));
                t.insert("frames".into(), Value::Int(0));
                t.insert("chain".into(), Value::Str("0000000000000000".into()));
                t.insert("ok".into(), Value::Bool(true));
            }
            Some(x) => {
                t.insert("mode".into(), Value::Sym(x.mode.name().into()));
                t.insert("frames".into(), Value::Int(x.frames.len() as i64));
                t.insert("chain".into(), Value::Str(format!("{:016x}", x.chain)));
                t.insert("mismatches".into(), Value::Int(x.mismatches.len() as i64));
                t.insert("ok".into(), Value::Bool(x.mismatches.is_empty()));
                t.insert("seed".into(), Value::Int(x.seed as i64));
                t.insert("path".into(), Value::Str(x.path.display().to_string()));
                if let Some(last) = x.mismatches.last() {
                    t.insert("error".into(), Value::Str(last.clone()));
                }
            }
        }
        t
    });
    Value::Table(std::rc::Rc::new(std::cell::RefCell::new(t)))
}

/// Однострочный статус для редактора и консоли: работает и без JSON.
pub fn status_line() -> String {
    with_state(|s| match s.as_ref() {
        None => "mode=none".to_string(),
        Some(x) => {
            let ok = x.mismatches.is_empty() && x.recorded_chain.is_none_or(|c| c == x.chain);
            format!(
                "mode={} frames={} chain={:016x} seed={} path={} {}",
                x.mode.name(),
                x.frames.len(),
                x.chain,
                x.seed,
                x.path.display(),
                if ok { "ok" } else { "MISMATCH" }
            )
        }
    })
}

/// Запись пройдена до конца — основной цикл может завершаться.
pub fn finished() -> bool {
    with_state(|s| s.as_ref().map(|x| x.done).unwrap_or(false))
}

/// Есть ли расхождения (кадры или итоговая цепочка) — для кода возврата.
pub fn diverged() -> bool {
    with_state(|s| {
        s.as_ref()
            .map(|x| !x.mismatches.is_empty() || x.recorded_chain.is_some_and(|c| c != x.chain))
            .unwrap_or(false)
    })
}

/// Записать файл записи по указанному пути (для натива replay-stop! → save).
pub fn save_to(session: &Session, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        }
    }
    std::fs::write(path, session.serialize()).map_err(|e| format!("{}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_вектор_и_чувствительность() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv1a(&[0u8, 0, 0, 0]), fnv1a(&[1u8, 0, 0, 0]));
    }

    #[test]
    fn chain_детерминирован_и_лавинен() {
        let a = chain_step(0, 0, 12345);
        let b = chain_step(0, 0, 12345);
        assert_eq!(a, b);
        assert_ne!(a, chain_step(0, 1, 12345));
        assert_ne!(a, chain_step(0, 0, 12346));
        let mut c = 0;
        for i in 0..64 {
            c = chain_step(c, i, i * 7919);
        }
        assert_ne!(c, 0);
    }

    #[test]
    fn запись_читается_обратно_байт_в_байт() {
        let mut s = Session::new_record("/tmp/x.rlrp", 42, 60.0);
        s.step(0, 0xdead_beef, vec!["key-down:w".into()]).unwrap();
        s.step(1, 0x00ff_00ff, vec![]).unwrap();
        let text = s.serialize();
        let back = Session::parse(&text).unwrap();
        assert_eq!(back.seed, 42);
        assert_eq!(back.frames, s.frames);
        // цепочка в файле — эталон; у воспроизведения она пересчитывается с нуля
        assert_eq!(back.recorded_chain, Some(s.chain));
        assert_eq!(back.chain, 0);
        assert_eq!(back.mode, Mode::Replay);
    }

    #[test]
    fn воспроизведение_ловит_расхождение_кадра() {
        let mut s = Session::new_record("/tmp/y.rlrp", 7, 60.0);
        s.step(0, 111, vec![]).unwrap();
        s.step(1, 222, vec![]).unwrap();
        let text = s.serialize();

        let mut rep = Session::parse(&text).unwrap();
        assert!(rep.step(0, 111, vec![]).is_ok());
        let err = rep.step(1, 999, vec![]).unwrap_err();
        assert!(err.contains("кадр 1"), "{}", err);
        assert_eq!(rep.mismatches.len(), 1);
    }

    #[test]
    fn воспроизведение_ловит_расхождение_ввода() {
        let mut s = Session::new_record("/tmp/z.rlrp", 7, 60.0);
        s.step(0, 5, vec!["key-down:w".into()]).unwrap();
        let mut rep = Session::parse(&s.serialize()).unwrap();
        let err = rep.step(0, 5, vec![]).unwrap_err();
        assert!(err.contains("ввод"), "{}", err);
    }

    #[test]
    fn ввод_кодируется_и_декодируется() {
        let v = Value::List(vec![Value::Sym("key-down".into()), Value::Str("w".into())]);
        let s = encode_input(&v).unwrap();
        assert_eq!(s, "key-down:w");
        match decode_input(&s).unwrap() {
            Value::List(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(&items[0], Value::Sym(k) if k == "key-down"));
                assert!(matches!(&items[1], Value::Str(k) if k == "w"));
            }
            _ => panic!("ожидался список"),
        }
    }

    #[test]
    fn запись_с_середины_сверяется_с_кадром_старта() {
        // живая запись из редактора: кадры 0..5 прошли до начала записи
        let mut rec = Session::new_record("/tmp/mid.rlrp", 99, 60.0);
        for f in 0..5u64 {
            rec.step(f, 1000 + f, vec![]).unwrap(); // до старта — не пишется
        }
        assert_eq!(rec.start_frame, 0, "запись с нуля пишет всё");
        assert_eq!(rec.frames.len(), 5);

        let mut late = Session::new_record("/tmp/mid2.rlrp", 99, 60.0);
        late.start_frame = 3; // как будто запись включили на кадре 3
        late.frames.clear();
        for f in 3..6u64 {
            late.step(f, 2000 + f, vec![]).unwrap();
        }
        assert_eq!(late.frames.len(), 3);
        let text = late.serialize();
        let mut rep = Session::parse(&text).unwrap();
        assert_eq!(rep.start_frame, 3);
        // кадры до старта записи в воспроизведении просто пропускаются
        assert!(rep.step(0, 424242, vec![]).is_ok());
        assert!(rep.step(3, 2003, vec![]).is_ok());
        assert!(rep.step(4, 2004, vec![]).is_ok());
        assert!(rep.step(5, 2005, vec![]).is_ok());
        assert_eq!(rep.chain, late.chain);
    }

    #[test]
    fn файл_без_магии_отвергается() {
        assert!(Session::parse("HELLO\nseed 1\n").is_err());
        assert!(Session::parse(&format!(
            "{}\nseed 1\nfps 60\nframes 0\nchain 0000000000000000\n",
            MAGIC
        ))
        .is_ok());
    }
}
