// src/hal/core.rs — Ядро HAL: состояние буферов, вывод кадров, аудио, base64 и растровые операции.

use super::types::{MonoClock, OutputMode, Tex};
use crate::gpu::{Backend, GpuResources};
use crate::interp::{fmt_value, num_val, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub struct Hal {
    pub w: usize,
    pub h: usize,
    pub fb: Vec<u8>,
    pub frame: u64,
    pub title: String,
    pub out: OutputMode,
    pub queue: Vec<Value>,
    pub start: MonoClock,
    pub mem: HashMap<u64, Rc<RefCell<Vec<u8>>>>,
    pub next_mem: u64,
    /// «GPU» фиксированного конвейера: z-буфер + свет + статистика (render3d.rs)
    pub r3: crate::render3d::R3,
    /// Реестр текстур (v0.3): id = индекс+1, 0 — «без текстуры»
    pub tex: Vec<Tex>,
    /// Активный рендер-бэкенд за фасадом (software | wgpu)
    pub backend: Backend,
    /// Менеджер GPU-ресурсов: книга хэндлов + счётчики для тестов на утечки
    pub res: GpuResources,
    /// Накопленное время GPU: (submit, readback) в нс — для --bench
    pub gpu_submit_ns: u128,
    pub gpu_readback_ns: u128,
    pub readback_requested: bool,
    pub audio: Arc<Mutex<crate::audio::AudioMixer>>,
    pub audio_device: Option<crate::audio::NativeAudioDevice>,
}

impl Hal {
    pub fn new(out: OutputMode, w: usize, h: usize) -> Hal {
        Self::with_backend(out, w, h, Backend::Software)
    }

    pub fn with_backend(out: OutputMode, w: usize, h: usize, backend: Backend) -> Hal {
        let w = w.max(8);
        let h = h.max(8);
        let audio = Arc::new(Mutex::new(crate::audio::AudioMixer::new()));
        let audio_device = if cfg!(test) {
            None
        } else {
            crate::audio::NativeAudioDevice::start(audio.clone())
        };
        let mut hal = Hal {
            w,
            h,
            fb: vec![0; w * h * 4],
            frame: 0,
            title: String::new(),
            out,
            queue: Vec::new(),
            start: MonoClock::now(),
            mem: HashMap::new(),
            next_mem: 1,
            r3: crate::render3d::R3::new(),
            tex: Vec::new(),
            backend,
            res: GpuResources::default(),
            gpu_submit_ns: 0,
            gpu_readback_ns: 0,
            readback_requested: true,
            audio,
            audio_device,
        };
        hal.fill_all(8, 8, 18);
        hal
    }

    // ——— диспетчеризация растровых команд в активный бэкенд ———

    /// gpu-clear: у software — прямая запись в fb, у wgpu — clear-пас.
    pub fn clear_all(&mut self, r: u8, g: u8, b: u8) {
        if let Backend::Wgpu(gpu) = &mut self.backend {
            gpu.clear(r, g, b);
            return;
        }
        self.fill_all(r, g, b);
    }

    /// 3D-очистка: у software — fb + zbuf, у wgpu — аппаратная очистка color + depth
    pub fn clear_3d(&mut self, r: u8, g: u8, b: u8) {
        self.r3.reset(self.w, self.h);
        self.r3.transq.clear();
        self.r3.blend = 1.0;
        self.r3.drawn = 0;
        self.r3.culled = 0;
        if let Backend::Wgpu(gpu) = &mut self.backend {
            gpu.clear_3d(r, g, b);
            return;
        }
        self.fill_all(r, g, b);
        self.r3.zbuf.fill(f32::INFINITY);
    }

    pub fn fill_all(&mut self, r: u8, g: u8, b: u8) {
        let n = self.w * self.h;
        for i in 0..n {
            let p = i * 4;
            self.fb[p] = r;
            self.fb[p + 1] = g;
            self.fb[p + 2] = b;
            self.fb[p + 3] = 255;
        }
    }

    /// gpu-fill-rect
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rect(&mut self, x: i64, y: i64, w: i64, h: i64, r: u8, g: u8, b: u8) {
        if let Backend::Wgpu(gpu) = &mut self.backend {
            gpu.fill_rect(x, y, w, h, r, g, b);
            return;
        }
        let x0 = (x.max(0) as usize).min(self.w);
        let y0 = (y.max(0) as usize).min(self.h);
        let x1 = ((x + w).max(0) as usize).min(self.w);
        let y1 = ((y + h).max(0) as usize).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for yy in y0..y1 {
            let row = yy * self.w;
            for xx in x0..x1 {
                let p = (row + xx) * 4;
                self.fb[p] = r;
                self.fb[p + 1] = g;
                self.fb[p + 2] = b;
                self.fb[p + 3] = 255;
            }
        }
    }

    /// gpu-fill-rect-a: заливка с альфа-блендингом (a 0..1: 0 — не трогаем,
    /// 1 — как fill_rect). Software-растр; в wgpu-режиме рисуется в fb
    /// оверлеев отложенного 2D-слоя.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rect_a(&mut self, x: i64, y: i64, w: i64, h: i64, r: u8, g: u8, b: u8, a: f32) {
        if a <= 0.0 {
            return;
        }
        if a >= 1.0 {
            self.fill_rect(x, y, w, h, r, g, b);
            return;
        }
        let x0 = (x.max(0) as usize).min(self.w);
        let y0 = (y.max(0) as usize).min(self.h);
        let x1 = ((x + w).max(0) as usize).min(self.w);
        let y1 = ((y + h).max(0) as usize).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let af = a.clamp(0.0, 1.0);
        for yy in y0..y1 {
            let row = yy * self.w;
            for xx in x0..x1 {
                let p = (row + xx) * 4;
                let blend = |src: u8, c: u8| -> u8 {
                    (src as f32 * (1.0 - af) + c as f32 * af)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                self.fb[p] = blend(self.fb[p], r);
                self.fb[p + 1] = blend(self.fb[p + 1], g);
                self.fb[p + 2] = blend(self.fb[p + 2], b);
                self.fb[p + 3] = 255;
            }
        }
    }

    /// gpu-light-spot: аддитивный экранный свет — радиальный градиент
    /// (фонарь, лампа, вспышка, взрыв). Спад (1−d²/r²) — гладкий к нулю
    /// на краю; интенсивность 1.0 ≈ +255 в центре.
    #[allow(clippy::too_many_arguments)]
    pub fn spot_light(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        intensity: f32,
        r: u8,
        g: u8,
        b: u8,
    ) {
        if radius <= 0.0 || intensity <= 0.0 {
            return;
        }
        let x0 = ((cx - radius).floor().max(0.0) as usize).min(self.w);
        let y0 = ((cy - radius).floor().max(0.0) as usize).min(self.h);
        let x1 = ((cx + radius).ceil().min(self.w as f32) as usize).min(self.w);
        let y1 = ((cy + radius).ceil().min(self.h as f32) as usize).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let rad2 = radius * radius;
        for yy in y0..y1 {
            let row = yy * self.w;
            for xx in x0..x1 {
                let dx = xx as f32 - cx;
                let dy = yy as f32 - cy;
                let d2 = dx * dx + dy * dy;
                if d2 >= rad2 {
                    continue;
                }
                let fall = 1.0 - d2 / rad2; // гладкий спад к нулю на краю
                let k = fall * fall * intensity;
                let p = (row + xx) * 4;
                let add = |src: u8, c: u8| -> u8 { (src as f32 + c as f32 * k).min(255.0) as u8 };
                self.fb[p] = add(self.fb[p], r);
                self.fb[p + 1] = add(self.fb[p + 1], g);
                self.fb[p + 2] = add(self.fb[p + 2], b);
                self.fb[p + 3] = 255;
            }
        }
    }

    /// gpu-blit: RGBA-пиксели 1:1, alpha=0 пропускается
    pub fn blit(&mut self, x: i64, y: i64, w: usize, h: usize, bytes: &[u8]) -> Result<(), String> {
        if let Backend::Wgpu(gpu) = &mut self.backend {
            return gpu.blit(x, y, w, h, bytes);
        }
        for row in 0..h {
            let py = y + row as i64;
            if py < 0 || py >= self.h as i64 {
                continue;
            }
            for col in 0..w {
                let px = x + col as i64;
                if px < 0 || px >= self.w as i64 {
                    continue;
                }
                let si = (row * w + col) * 4;
                if si + 3 >= bytes.len() {
                    continue;
                }
                if bytes[si + 3] == 0 {
                    continue; // прозрачный пиксель
                }
                let di = (py as usize * self.w + px as usize) * 4;
                self.fb[di] = bytes[si];
                self.fb[di + 1] = bytes[si + 1];
                self.fb[di + 2] = bytes[si + 2];
                self.fb[di + 3] = 255;
            }
        }
        Ok(())
    }

    /// gpu-blit-tex: спрайт из реестра текстур (nearest, тинт, масштаб).
    /// Dest-прямоугольник считается одинаково для обоих бэкендов.
    pub fn blit_tex(
        &mut self,
        id: i64,
        x: i64,
        y: i64,
        scale: f32,
        tint: [f32; 3],
    ) -> Result<(), String> {
        let t = self
            .tex
            .get((id.max(1) - 1) as usize)
            .ok_or("gpu-blit-tex: нет такой текстуры")?
            .clone();
        if t.w == 0 || t.h == 0 {
            return Err("gpu-blit-tex: текстура освобождена".into());
        }
        let (tw, th) = (t.w as i64, t.h as i64);
        let dw = ((tw as f32 * scale).round().max(1.0)) as i64;
        let dh = ((th as f32 * scale).round().max(1.0)) as i64;
        if let Backend::Wgpu(gpu) = &mut self.backend {
            return gpu.blit_tex(id, x, y, dw, dh, tint);
        }
        for row in 0..dh {
            let py = y + row;
            if py < 0 || py >= self.h as i64 {
                continue;
            }
            let sy = ((row as f32 / scale) as usize).min(t.w - 1);
            for col in 0..dw {
                let pxx = x + col;
                if pxx < 0 || pxx >= self.w as i64 {
                    continue;
                }
                let sx = ((col as f32 / scale) as usize).min(t.w - 1);
                let si = (sy * t.w + sx) * 4;
                let a = t.px[si + 3];
                if a == 0 {
                    continue; // прозрачный пиксель
                }
                let di = (py as usize * self.w + pxx as usize) * 4;
                self.fb[di] = ((t.px[si] as f32 * tint[0]) as u8).clamp(0, 255);
                self.fb[di + 1] = ((t.px[si + 1] as f32 * tint[1]) as u8).clamp(0, 255);
                self.fb[di + 2] = ((t.px[si + 2] as f32 * tint[2]) as u8).clamp(0, 255);
                self.fb[di + 3] = 255;
            }
        }
        Ok(())
    }

    /// wgpu: дослать кадр на GPU и при readback=true скомпозитить его в fb
    /// (PNG/ASCII/редактор читают fb — путь вывода не меняется). Software: no-op.
    /// Возвращает (submit_ns, readback_ns) — для профилирования.
    pub fn flush_gpu(&mut self, readback: bool) -> (u128, u128) {
        // swap, чтобы borrow бэкенда не пересекался с &mut fb
        match std::mem::replace(&mut self.backend, Backend::Software) {
            Backend::Wgpu(mut b) => {
                let (w, h) = (self.w, self.h);
                let t = b.flush(&mut self.fb, w, h, readback);
                self.gpu_submit_ns += t.0;
                self.gpu_readback_ns += t.1;
                self.backend = Backend::Wgpu(b);
                t
            }
            other => {
                self.backend = other;
                (0, 0)
            }
        }
    }

    /// window-create: смена размера экрана (fb + бэкенд + 3D-«GPU»)
    pub fn resize_window(&mut self, w: usize, h: usize) {
        if w > 0 && h > 0 && w * h <= 4_000_000 {
            self.w = w;
            self.h = h;
            self.fb = vec![0; w * h * 4];
        }
        if let Backend::Wgpu(gpu) = &mut self.backend {
            gpu.resize(self.w, self.h);
        }
        self.r3.reset(self.w, self.h);
    }

    // ——— менеджер GPU-ресурсов: реестр текстур в обеих ветках ———

    /// texture-load / texture-upload: хэндл в книгу ресурсов + загрузка на бэкенд
    pub fn tex_register(&mut self, id: i64, w: usize, h: usize, px: &[u8]) {
        let _ = (w, h); // размеры нужны только wgpu-стороне (создание текстуры)
        self.res.register(id, px.len());
        if let Some(t) = self.tex.get_mut((id - 1) as usize) {
            if t.mips.is_empty() && t.w > 0 && t.h > 0 {
                t.generate_mips();
            }
        }
        if let Backend::Wgpu(gpu) = &mut self.backend {
            gpu.tex_upload(id, w, h, px);
        }
    }

    /// texture-free: освободить слот реестра + хэндл бэкенда
    pub fn tex_free(&mut self, id: i64) -> bool {
        let freed = self.res.free(id);
        if id >= 1 && id <= self.tex.len() as i64 {
            self.tex[(id - 1) as usize] = Tex::empty();
        }
        if let Backend::Wgpu(gpu) = &mut self.backend {
            gpu.tex_free(id);
        }
        freed
    }

    /// hot-reload: освободить все текстуры, кроме id из keep (на них ссылаются
    /// persistent-значения), срезать пустые хвостовые слоты реестра.
    /// Возвращает число освобождённых хэндлов.
    pub fn invalidate_textures_except(&mut self, keep: &[i64]) -> usize {
        let freed = self.res.invalidate_except(keep);
        if let Backend::Wgpu(gpu) = &mut self.backend {
            gpu.clear_mesh_cache();
            for id in 1..=self.tex.len() as i64 {
                if !keep.contains(&id) {
                    gpu.tex_free(id);
                }
            }
        }
        // нулевые слоты реестра для освобождённых id + срезка хвоста:
        // px старых текстур больше не держится в памяти (в v0.3 они оставались
        // до рестарта — «утечка 64 КБ/текстуру за reload», теперь исправлено)
        for (i, t) in self.tex.iter_mut().enumerate() {
            let id = (i + 1) as i64;
            if !keep.contains(&id) {
                *t = Tex::empty();
            }
        }
        while self.tex.last().is_some_and(|t| t.w == 0) {
            self.tex.pop();
        }
        freed
    }
}

pub fn f2b(x: f64) -> u8 {
    (x * 255.0).round().clamp(0.0, 255.0) as u8
}

// ——— этап 8 «модели из интернета»: сырые данные для glTF-пайплайна ———
// Ноль зависимостей, как png.rs/json.rs: форматы «трубы», не игровая логика.
// Lisp-загрузчик (assets/lisp/load-gltf.lisp) решает, что с ними делать.

/// Base64-декодер (RFC 4648, стандартный алфавит). Принимает и data-URI
/// («data:application/octet-stream;base64,XXXX») — всё до первой запятой
/// отбрасывается; в чистом base64 запятой не бывает. Пробельные символы
/// (переносы строк) пропускаются. Ошибка — на недопустимом символе,
/// «данные после =» и неканоничном хвосте.
pub fn base64_decode(src: &str) -> Result<Vec<u8>, String> {
    fn val6(c: u8) -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!(
                "base64: недопустимый символ {:?} (код {})",
                c as char, c
            )),
        }
    }
    let mut s = src;
    // data-URI: «data:image/png;base64,....» → payload после запятой
    if s.trim_start().starts_with("data:") {
        match s.find(',') {
            Some(i) => s = &s[i + 1..],
            None => return Err("base64: data-URI без запятой".into()),
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    let mut pad = 0usize;
    for &b in s.as_bytes() {
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            b'=' => {
                pad += 1;
            }
            _ => {
                if pad > 0 {
                    return Err("base64: данные после «=»".into());
                }
                acc = (acc << 6) | val6(b)?;
                nbits += 6;
                if nbits >= 8 {
                    nbits -= 8;
                    out.push(((acc >> nbits) & 0xFF) as u8);
                    acc &= (1 << nbits) - 1;
                }
            }
        }
    }
    // каноничность: хвост короче 8 бит обязан быть нулевыми битами паддинга;
    // 6 бит остатка — «висячий» символ (всего один в последней группе)
    if nbits >= 6 {
        return Err("base64: длина не кратна группе 4 символов".into());
    }
    if nbits > 0 && acc != 0 {
        return Err("base64: ненулевые биты после паддинга".into());
    }
    Ok(out)
}

pub fn str_val(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Sym(s) => s.to_string(),
        other => fmt_value(other),
    }
}

pub fn ascii_frame(hal: &Hal) -> String {
    let cols = 100usize;
    let rows = (((hal.h as f64 / hal.w as f64) * cols as f64) * 0.5)
        .round()
        .max(8.0) as usize;
    let cw = (hal.w / cols).max(1);
    let ch = (hal.h / rows).max(1);
    let mut s = String::from("\x1b[H");
    for ry in 0..rows {
        for rx in 0..cols {
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut n = 0u32;
            let y1 = ((ry + 1) * ch).min(hal.h);
            let x1 = ((rx + 1) * cw).min(hal.w);
            for yy in (ry * ch)..y1 {
                for xx in (rx * cw)..x1 {
                    let p = (yy * hal.w + xx) * 4;
                    r += hal.fb[p] as u32;
                    g += hal.fb[p + 1] as u32;
                    b += hal.fb[p + 2] as u32;
                    n += 1;
                }
            }
            if n == 0 {
                s.push(' ');
                continue;
            }
            let r = (r / n) as u8;
            let g = (g / n) as u8;
            let b = (b / n) as u8;
            let lum = r as u32 * 30 + g as u32 * 59 + b as u32 * 11;
            if lum < 40 {
                s.push(' ');
            } else {
                s.push_str(&format!("\x1b[38;2;{};{};{}m\u{2588}", r, g, b));
            }
        }
        s.push_str("\x1b[0m\r\n");
    }
    s.push_str(&format!(
        "\x1b[K  rlisp-engine | кадр {} | {}x{}\r\n",
        hal.frame, hal.w, hal.h
    ));
    s
}

pub fn to_rgb(v: &Value) -> Result<(f32, f32, f32), String> {
    match v {
        Value::List(l) if l.len() >= 3 => Ok((
            num_val(&l[0])? as f32,
            num_val(&l[1])? as f32,
            num_val(&l[2])? as f32,
        )),
        Value::Vector(r) if r.borrow().len() >= 3 => {
            let b = r.borrow();
            Ok((
                num_val(&b[0])? as f32,
                num_val(&b[1])? as f32,
                num_val(&b[2])? as f32,
            ))
        }
        _ => Err("ожидался цвет (r g b)".into()),
    }
}

pub fn mem_block(hal: &Hal, v: &Value) -> Result<Rc<RefCell<Vec<u8>>>, String> {
    match v {
        Value::Ptr(id) => hal
            .mem
            .get(id)
            .cloned()
            .ok_or_else(|| format!("mem: неизвестный указатель {}", id)),
        other => Err(format!(
            "mem: нужен указатель из mem-alloc: {}",
            fmt_value(other)
        )),
    }
}
