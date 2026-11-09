// src/gpu.rs — GPU-бэкенд за фасадом HAL (этап 1 роадмапа: «HAL — тонкий и быстрый»).
//
// Философия соблюдена: Lisp-код НЕ ЗНАЕТ о бэкендах. Нативы gpu-clear /
// gpu-fill-rect / gpu-blit / gpu-blit-tex / gpu-present идут в Hal, а Hal
// направляет их в активный бэкенд (enum Backend + trait GpuBackend):
//
//   Backend::Software — программный растр (дефолт, НОЛЬ зависимостей):
//                       команды пишут прямо в Hal::fb, как в v0.1–v0.3;
//   Backend::Wgpu     — wgpu (только с фичей `wgpu`, опциональная зависимость):
//                       команды уходят в offscreen-текстуру (RGBA8), при
//                       flush/present кадр копируется с GPU (map-read) в
//                       Hal::fb — поэтому PNG/ASCII/редактор/MCP-вывод работают
//                       без единого изменения.
//
// Честно о покрытии (этап 1): wgpu закрывает 2D-растр (clear / fill-rect /
// blit / blit-tex / present). 3D-конвейер (render3d.rs: T&L, z-буфер, батчи)
// остаётся программным — он рисует в Hal::fb. Чтобы порядок художника совпадал
// с программным растром пиксель-в-пиксель, перед любой 3D-командой отложенные
// 2D-результаты wgpu сбрасываются в fb (Hal::flush_gpu), а flush композитит
// GPU-слой поверх fb по альфе-покрытию (см. composite_readback).
//
// Ресурсы: GpuResources — «книга хэндлов» без устройства (счётчики для тестов
// на утечки: 100 create/reload циклов без роста). Hot-reload вызывает
// Hal::invalidate_textures_except — старые текстуры освобождаются (в v0.3 они
// утекали до рестарта, см. README «Честные отличия» — теперь исправлено).

use std::collections::HashMap;

// ————————————————————————————————————————————————————————————————————————
// Фасад: enum Backend + trait GpuBackend
// ————————————————————————————————————————————————————————————————————————

pub enum Backend {
    Software,
    Wgpu(Box<dyn GpuBackend>),
}

/// То, что умеет каждый GPU-бэкенд за фасадом HAL. Профиль API — как у
/// программного растра v0.1 (gpu-clear/fill-rect/blit/present), плюс реестр
/// текстур и resize.
pub trait GpuBackend {
    /// человекочитаемое имя адаптера для баннера/логов
    fn adapter_info(&self) -> String;
    /// пересоздать offscreen-таргет под новый размер (window-create)
    fn resize(&mut self, w: usize, h: usize);
    /// gpu-clear: очистка цветом, alpha = 1 (полное покрытие кадра)
    fn clear(&mut self, r: u8, g: u8, b: u8);
    /// gpu-fill-rect
    #[allow(clippy::too_many_arguments)]
    fn fill_rect(&mut self, x: i64, y: i64, w: i64, h: i64, r: u8, g: u8, b: u8);
    /// gpu-blit: пиксели RGBA 1:1, alpha=0 пропускается
    fn blit(&mut self, x: i64, y: i64, w: usize, h: usize, bytes: &[u8]) -> Result<(), String>;
    /// gpu-blit-tex: текстурный спрайт (nearest, тинт), dest-прямоугольник
    /// считает Hal (те же формулы, что у программного растра)
    fn blit_tex(
        &mut self,
        id: i64,
        x: i64,
        y: i64,
        dw: i64,
        dh: i64,
        tint: [f32; 3],
    ) -> Result<(), String>;
    /// реестр текстур: загрузка/перезагрузка на бэкенд
    fn tex_upload(&mut self, id: i64, w: usize, h: usize, px: &[u8]);
    fn tex_free(&mut self, id: i64);
    /// дослать команды; при readback — композит кадра в fb.
    /// Возвращает (submit_ns, readback_ns) — профилирование --bench.
    fn flush(&mut self, fb: &mut [u8], w: usize, h: usize, readback: bool) -> (u128, u128);
}

/// Выбор бэкенда для --wgpu: без фичи или без адаптера — честный откат на
/// программный растр (все скрипты продолжают работать). Выделено в функцию,
/// чтобы фолбэк-логика покрывалась тестом без устройства.
pub fn fallback_warning(probe: &Result<Box<dyn GpuBackend>, String>) -> Option<String> {
    match probe {
        Ok(_) => None,
        Err(e) => Some(format!("--wgpu: {} — откат на программный растр", e)),
    }
}

/// Создать wgpu-бэкенд. Без фичи `wgpu` — понятная ошибка (откат на софт).
#[cfg(not(feature = "wgpu"))]
pub fn create_wgpu_backend(_w: usize, _h: usize) -> Result<Box<dyn GpuBackend>, String> {
    Err("бинарь собран без GPU-бэкенда: cargo build --release --features wgpu".into())
}

#[cfg(feature = "wgpu")]
pub fn create_wgpu_backend(w: usize, h: usize) -> Result<Box<dyn GpuBackend>, String> {
    Ok(Box::new(WgpuRenderer::new(w.max(8), h.max(8))?))
}

// ————————————————————————————————————————————————————————————————————————
// Менеджер GPU-ресурсов: книга хэндлов + счётчики (работает без устройства)
// ————————————————————————————————————————————————————————————————————————

/// Счётчики ресурсов — их assert-ят тесты на отсутствие утечек.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct GpuStats {
    pub tex_created: u64,
    pub tex_freed: u64,
    /// текстур, освобождённых при hot-reload (инвалидация)
    pub reload_invalidated: u64,
    /// hot-reload циклов, прошедших через invalidate
    pub reload_cycles: u64,
}

impl GpuStats {
    /// живые хэндлы: тест «нет утечек» = tex_created == tex_freed в конце
    pub fn tex_live(&self) -> i64 {
        self.tex_created as i64 - self.tex_freed as i64
    }
}

#[derive(Clone, Debug)]
struct Slot {
    /// поколение: +1 при каждой перезагрузке того же id
    gen: u64,
    /// суммарный байт пикселей живой текстуры (рост = утечка)
    bytes: usize,
}

/// Книга GPU-ресурсов. Зеркалит реестр hal.tex (id = индекс+1, 0 — «нет»),
/// но не держит пиксели — только метаданные и счётчики. На wgpu-бэкенде
/// рядом с ней живёт HashMap<id, wgpu::Texture> (см. WgpuRenderer).
#[derive(Default)]
pub struct GpuResources {
    slots: HashMap<i64, Slot>,
    pub stats: GpuStats,
}

impl GpuResources {
    /// текстура создана/перезагружена под тем же id. Перезагрузка живого id
    /// замещает старое поколение (free + create) — счётчики честные: в конце
    /// tex_created - tex_freed == число живых хэндлов.
    pub fn register(&mut self, id: i64, bytes: usize) {
        match self.slots.remove(&id) {
            Some(old) => {
                self.stats.tex_freed += 1; // старое поколение замещено
                self.slots.insert(
                    id,
                    Slot {
                        gen: old.gen + 1,
                        bytes,
                    },
                );
            }
            None => {
                self.slots.insert(id, Slot { gen: 1, bytes });
            }
        }
        self.stats.tex_created += 1;
    }

    /// явное освобождение (texture-free / GPU-сторона). Возвращает true,
    /// если хэндл был жив.
    pub fn free(&mut self, id: i64) -> bool {
        if self.slots.remove(&id).is_some() {
            self.stats.tex_freed += 1;
            true
        } else {
            false
        }
    }

    pub fn live(&self) -> usize {
        self.slots.len()
    }

    /// суммарный байт пикселей живых текстур (рост = утечка)
    pub fn bytes(&self) -> usize {
        self.slots.values().map(|s| s.bytes).sum()
    }

    pub fn generation(&self, id: i64) -> u64 {
        self.slots.get(&id).map(|s| s.gen).unwrap_or(0)
    }

    /// hot-reload: освободить всё, КРОМЕ id из keep (id, на которые ссылаются
    /// persistent-значения). Возвращает число освобождённых.
    pub fn invalidate_except(&mut self, keep: &[i64]) -> usize {
        self.stats.reload_cycles += 1;
        let before = self.slots.len();
        self.slots.retain(|id, _| keep.contains(id));
        let freed = (before - self.slots.len()) as u64;
        self.stats.tex_freed += freed;
        self.stats.reload_invalidated += freed;
        freed as usize
    }

    pub fn reset(&mut self) {
        self.slots.clear();
        self.stats = GpuStats::default();
    }
}

/// Композит GPU-кадра в fb по альфе-покрытию (вызывается из readback wgpu;
/// без фичи `wgpu` живёт для тестов композита):
/// пиксель с alpha==255 заменяет fb, прозрачный — оставляет fb как есть
/// (там, как правило, результат программного 3D-конвейера). `pad` — длина
/// строки в буфере GPU (выравнивание 256 байт), data — весь буфер.
#[cfg_attr(not(feature = "wgpu"), allow(dead_code))]
pub fn composite_readback(fb: &mut [u8], w: usize, h: usize, pad: usize, data: &[u8]) {
    for py in 0..h {
        let src = &data[py * pad..py * pad + w * 4];
        for px in 0..w {
            let s = px * 4;
            if src[s + 3] == 255 {
                let d = (py * w + px) * 4;
                fb[d] = src[s];
                fb[d + 1] = src[s + 1];
                fb[d + 2] = src[s + 2];
                fb[d + 3] = 255;
            }
        }
    }
}

/// Собрать keep-список id (все Int в значениях) для invalidate_except:
/// рекурсивный обход persistent-значений (списки/векторы/таблицы).
pub fn collect_int_refs(v: &crate::interp::Value, out: &mut Vec<i64>, depth: u32) {
    if depth > 8 {
        return;
    }
    use crate::interp::Value;
    match v {
        Value::Int(i) => out.push(*i),
        Value::List(l) => {
            for x in l {
                collect_int_refs(x, out, depth + 1);
            }
        }
        Value::Vector(r) => {
            for x in r.borrow().iter() {
                collect_int_refs(x, out, depth + 1);
            }
        }
        Value::Table(t) => {
            for x in t.borrow().values() {
                collect_int_refs(x, out, depth + 1);
            }
        }
        _ => {}
    }
}

// ————————————————————————————————————————————————————————————————————————
// wgpu-бэкенд (фича `wgpu`): offscreen-таргет → readback → Hal::fb
// ————————————————————————————————————————————————————————————————————————

#[cfg(feature = "wgpu")]
const SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tint: vec3<f32>,
) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.tint = tint;
    return out;
}

@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var tex: texture_2d<f32>;

@fragment
fn fs_solid(@location(0) uv: vec2<f32>, @location(1) tint: vec3<f32>) -> @location(0) vec4<f32> {
    let _ = uv;
    return vec4<f32>(tint, 1.0);
}

@fragment
fn fs_tex(@location(0) uv: vec2<f32>, @location(1) tint: vec3<f32>) -> @location(0) vec4<f32> {
    let t = textureSample(tex, samp, uv);
    if (t.a == 0.0) { discard; } // как в программном blit: alpha=0 не пишем
    return vec4<f32>(t.rgb * tint, 1.0);
}
"#;

/// Мини-исполнитель фьючерсов (как pollster, но без зависимостей): wgpu на
/// нативе завершает фьючерсы внутренними потоками, достаточно спин-поллинга.
#[cfg(feature = "wgpu")]
fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn noop(_: *const ()) {}
    fn cloner(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(cloner, noop, noop, noop);

    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

#[cfg(feature = "wgpu")]
struct Quad {
    // 6 вершин × 8 f32 (pos2 uv2 tint3 pad)
    data: [f32; 48],
}

#[cfg(feature = "wgpu")]
#[allow(clippy::too_many_arguments)]
fn quad_verts(
    x0: i64,
    y0: i64,
    dw: i64,
    dh: i64,
    w: usize,
    h: usize,
    uv0: [f32; 2],
    uv1: [f32; 2],
    tint: [f32; 3],
) -> Quad {
    // пиксели → NDC (y вниз, как в фреймбуфере)
    let ndc_x = |px: f32| px / w as f32 * 2.0 - 1.0;
    let ndc_y = |py: f32| 1.0 - py / h as f32 * 2.0;
    let (ax0, ay0) = (ndc_x(x0 as f32), ndc_y(y0 as f32));
    let (ax1, ay1) = (ndc_x((x0 + dw) as f32), ndc_y((y0 + dh) as f32));
    let [u0, v0] = uv0;
    let [u1, v1] = uv1;
    let mut v: Vec<f32> = Vec::with_capacity(48);
    let mut push = |x: f32, y: f32, u: f32, vv: f32| {
        v.extend_from_slice(&[x, y, u, vv, tint[0], tint[1], tint[2], 0.0]);
    };
    // два треугольника: TL TR BR | TL BR BL
    push(ax0, ay0, u0, v0);
    push(ax1, ay0, u1, v0);
    push(ax1, ay1, u1, v1);
    push(ax0, ay0, u0, v0);
    push(ax1, ay1, u1, v1);
    push(ax0, ay1, u0, v1);
    let mut data = [0f32; 48];
    data.copy_from_slice(&v);
    Quad { data }
}

#[cfg(feature = "wgpu")]
pub struct WgpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: String,

    // offscreen-таргет: RGB — цвет, alpha — маска покрытия GPU-командами
    target: wgpu::Texture,
    target_w: usize,
    target_h: usize,
    readback: wgpu::Buffer,
    readback_pad: usize,

    vbuf: wgpu::Buffer,
    vcap: usize, // байт

    pipeline_solid: wgpu::RenderPipeline,
    pipeline_tex: wgpu::RenderPipeline,
    tex_bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,

    /// реестр текстур бэкенда: id → (текстура, w, h)
    texs: HashMap<i64, (wgpu::Texture, u32, u32)>,
    /// скрэтч-текстура для gpu-blit (переиспользуется при том же размере)
    scratch: Option<(usize, usize, wgpu::Texture)>,

    enc: Option<wgpu::CommandEncoder>,
    /// байт вершин, уже записанных в vbuf в текущем кадре
    vused: usize,
    /// pending-команды в кадре (для gpu_frames статистики)
    pending: u64,
}

#[cfg(feature = "wgpu")]
impl WgpuRenderer {
    pub fn new(w: usize, h: usize) -> Result<WgpuRenderer, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| {
            "wgpu-адаптер недоступен (headless/CI/VM без GPU: ни Vulkan, ни GL)".to_string()
        })?;
        let info = adapter.get_info();
        let adapter_desc = format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type);
        let (device, queue) = block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("rlisp-hal"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        ))
        .map_err(|e| format!("wgpu: устройство не создано: {}", e))?;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rlisp-hal-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let tex_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rlisp-tex"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let vlayout = wgpu::VertexBufferLayout {
            array_stride: 32,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 16,
                    shader_location: 2,
                },
            ],
        };
        let make_pipeline = |fs: &str, layout: &wgpu::PipelineLayout| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fs),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: "vs_main",
                    compilation_options: Default::default(),
                    buffers: &[vlayout.clone()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: fs,
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        // блендинг выключен: покрытие учитывается на композите
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            })
        };
        let layout_solid = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rlisp-solid"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let layout_tex = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rlisp-tex-pipe"),
            bind_group_layouts: &[&tex_bind_layout],
            push_constant_ranges: &[],
        });
        let pipeline_solid = make_pipeline("fs_solid", &layout_solid);
        let pipeline_tex = make_pipeline("fs_tex", &layout_tex);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rlisp-nearest"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // offscreen-таргет и readback создаются до перемещения device в структуру
        let target = device.create_texture(&Self::target_desc(w, h));
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rlisp-readback"),
            size: (Self::pad_rows(w) * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let vbuf_dummy = Self::dummy_buffer(&device);
        let readback_pad = Self::pad_rows(w);
        let mut r = WgpuRenderer {
            device,
            queue,
            adapter: adapter_desc,
            target,
            target_w: w,
            target_h: h,
            readback,
            readback_pad,
            vbuf: vbuf_dummy,
            vcap: 0,
            pipeline_solid,
            pipeline_tex,
            tex_bind_layout,
            sampler,
            texs: HashMap::new(),
            scratch: None,
            enc: None,
            vused: 0,
            pending: 0,
        };
        r.vbuf = r.make_vbuf();
        Ok(r)
    }

    fn dummy_buffer(device: &wgpu::Device) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rlisp-empty"),
            size: 4,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn make_vbuf(&self) -> wgpu::Buffer {
        // стартовая ёмкость: 1 МБ ≈ 32 тыс. вершин (растёт по потребности)
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rlisp-quads"),
            size: 1 << 20,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn pad_rows(w: usize) -> usize {
        (w * 4).div_ceil(256) * 256
    }

    fn target_desc(w: usize, h: usize) -> wgpu::TextureDescriptor<'static> {
        wgpu::TextureDescriptor {
            label: Some("rlisp-offscreen"),
            size: wgpu::Extent3d {
                width: w as u32,
                height: h as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        }
    }

    fn make_readback(&self, w: usize, h: usize) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rlisp-readback"),
            size: (Self::pad_rows(w) * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn ensure_enc(&mut self) -> &mut wgpu::CommandEncoder {
        if self.enc.is_none() {
            self.enc = Some(
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("rlisp-frame"),
                    }),
            );
        }
        self.enc.as_mut().unwrap()
    }

    fn ensure_vcap(&mut self, need: usize) {
        if self.vused + need <= self.vcap {
            return;
        }
        let mut cap = self.vcap.max(1 << 20); // старт: 1 МБ ≈ 32 тыс. вершин
        while cap < self.vused + need {
            cap *= 2;
        }
        self.vbuf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rlisp-quads"),
            size: cap as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.vcap = cap;
        self.vused = 0;
    }

    /// один квад на пайплайне; bind — для текстурного варианта
    fn draw_quad(
        &mut self,
        q: Quad,
        solid: bool,
        bind: Option<&wgpu::BindGroup>,
        load: wgpu::LoadOp<wgpu::Color>,
    ) {
        self.ensure_vcap(48 * 4);
        self.queue
            .write_buffer(&self.vbuf, self.vused as u64, bytemuck_bytes(&q.data));
        let view = self
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
        // ссылки до заимствования энкодера — энкодер забираем из self,
        // чтобы borrow'и не пересекались
        let pipe = if solid {
            &self.pipeline_solid
        } else {
            &self.pipeline_tex
        };
        let range = self.vused as u64..(self.vused + 48 * 4) as u64;
        let mut enc = self.enc.take().unwrap_or_else(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("rlisp-frame"),
                })
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rlisp-quad"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipe);
            if let Some(bg) = bind {
                pass.set_bind_group(0, bg, &[]);
            }
            pass.set_vertex_buffer(0, self.vbuf.slice(range));
            pass.draw(0..6, 0..1);
        }
        self.enc = Some(enc);
        self.vused += 48 * 4;
        self.pending += 1;
    }

    /// общая запись draw-команды на GPU
    fn record(
        &mut self,
        target_w: usize,
        target_h: usize,
        q: Quad,
        solid: bool,
        bind: Option<&wgpu::BindGroup>,
    ) {
        self.draw_quad(q, solid, bind, wgpu::LoadOp::Load);
        let _ = (target_w, target_h);
    }
}

#[cfg(feature = "wgpu")]
fn bytemuck_bytes(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}

#[cfg(feature = "wgpu")]
impl GpuBackend for WgpuRenderer {
    fn adapter_info(&self) -> String {
        self.adapter.clone()
    }

    fn resize(&mut self, w: usize, h: usize) {
        let (w, h) = (w.max(8), h.max(8));
        if w == self.target_w && h == self.target_h {
            return;
        }
        self.target = self.device.create_texture(&Self::target_desc(w, h));
        self.target_w = w;
        self.target_h = h;
        self.readback_pad = Self::pad_rows(w);
        self.readback = self.make_readback(w, h);
        self.enc = None;
        self.vused = 0;
        self.pending = 0;
    }

    fn clear(&mut self, r: u8, g: u8, b: u8) {
        let color = wgpu::Color {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a: 1.0, // полное покрытие — как программный gpu-clear
        };
        let view = self
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
        let enc = self.ensure_enc();
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rlisp-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        }); // пустой pass с loadOp=Clear — и есть очистка
        self.pending += 1;
    }

    fn fill_rect(&mut self, x: i64, y: i64, w: i64, h: i64, r: u8, g: u8, b: u8) {
        // тот же клип, что у программного растра
        let x0 = (x.max(0) as usize).min(self.target_w);
        let y0 = (y.max(0) as usize).min(self.target_h);
        let x1 = ((x + w).max(0) as usize).min(self.target_w);
        let y1 = ((y + h).max(0) as usize).min(self.target_h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let q = quad_verts(
            x0 as i64,
            y0 as i64,
            (x1 - x0) as i64,
            (y1 - y0) as i64,
            self.target_w,
            self.target_h,
            [0.0, 0.0],
            [0.0, 0.0],
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
        );
        self.record(self.target_w, self.target_h, q, true, None);
    }

    fn blit(&mut self, x: i64, y: i64, w: usize, h: usize, bytes: &[u8]) -> Result<(), String> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        // скрэтч-текстура под размер спрайта (переиспользуем)
        let reuse = matches!(&self.scratch, Some((sw, sh, _)) if *sw == w && *sh == h);
        if !reuse {
            let t = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rlisp-blit-scratch"),
                size: wgpu::Extent3d {
                    width: w as u32,
                    height: h as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.scratch = Some((w, h, t));
        }
        let Some((_, _, t)) = &self.scratch else {
            return Err("blit: нет scratch".into());
        };
        // короткий буфер дописываем прозрачным — как программный blit
        let need = w * h * 4;
        let owned: Vec<u8>;
        let px: &[u8] = if bytes.len() >= need {
            &bytes[..need]
        } else {
            owned = {
                let mut v = bytes.to_vec();
                v.resize(need, 0);
                v
            };
            &owned
        };
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: t,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            px,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some((w * 4) as u32),
                rows_per_image: Some(h as u32),
            },
            wgpu::Extent3d {
                width: w as u32,
                height: h as u32,
                depth_or_array_layers: 1,
            },
        );
        let view = t.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rlisp-blit-bind"),
            layout: &self.tex_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        // клип деста + сдвиг UV (1:1, как программный blit)
        let (tw, th) = (self.target_w, self.target_h);
        let dx0 = x.max(0);
        let dy0 = y.max(0);
        let dx1 = (x + w as i64).min(tw as i64);
        let dy1 = (y + h as i64).min(th as i64);
        if dx0 >= dx1 || dy0 >= dy1 {
            return Ok(());
        }
        let u0 = (dx0 - x) as f32 / w as f32;
        let v0 = (dy0 - y) as f32 / h as f32;
        let u1 = (dx1 - x) as f32 / w as f32;
        let v1 = (dy1 - y) as f32 / h as f32;
        let q = quad_verts(
            dx0,
            dy0,
            dx1 - dx0,
            dy1 - dy0,
            tw,
            th,
            [u0, v0],
            [u1, v1],
            [1.0, 1.0, 1.0],
        );
        self.record(tw, th, q, false, Some(&bg));
        Ok(())
    }

    fn blit_tex(
        &mut self,
        id: i64,
        x: i64,
        y: i64,
        dw: i64,
        dh: i64,
        tint: [f32; 3],
    ) -> Result<(), String> {
        let Some((t, tex_w, tex_h)) = self.texs.get(&id) else {
            return Err(format!("gpu-blit-tex: нет текстуры {} на GPU", id));
        };
        let _ = (tex_w, tex_h); // размеры текстуры — uv уже нормирован по dw/dh
        let view = t.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rlisp-blit-tex-bind"),
            layout: &self.tex_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        let (dx0, dy0) = (x.max(0), y.max(0));
        let (dx1, dy1) = (
            (x + dw).min(self.target_w as i64),
            (y + dh).min(self.target_h as i64),
        );
        if dx0 >= dx1 || dy0 >= dy1 || dw <= 0 || dh <= 0 {
            return Ok(());
        }
        let u0 = (dx0 - x) as f32 / dw as f32;
        let v0 = (dy0 - y) as f32 / dh as f32;
        let u1 = (dx1 - x) as f32 / dw as f32;
        let v1 = (dy1 - y) as f32 / dh as f32;
        let q = quad_verts(
            dx0,
            dy0,
            dx1 - dx0,
            dy1 - dy0,
            self.target_w,
            self.target_h,
            [u0, v0],
            [u1, v1],
            tint,
        );
        self.record(self.target_w, self.target_h, q, false, Some(&bg));
        Ok(())
    }

    fn tex_upload(&mut self, id: i64, w: usize, h: usize, px: &[u8]) {
        let t = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rlisp-tex"),
            size: wgpu::Extent3d {
                width: w as u32,
                height: h as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &t,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            px,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some((w * 4) as u32),
                rows_per_image: Some(h as u32),
            },
            wgpu::Extent3d {
                width: w as u32,
                height: h as u32,
                depth_or_array_layers: 1,
            },
        );
        self.texs.insert(id, (t, w as u32, h as u32));
    }

    fn tex_free(&mut self, id: i64) {
        self.texs.remove(&id); // wgpu::Texture удаляется вместе со слотом
    }

    fn flush(&mut self, fb: &mut [u8], w: usize, h: usize, readback: bool) -> (u128, u128) {
        let t0 = std::time::Instant::now();
        if let Some(enc) = self.enc.take() {
            self.queue.submit([enc.finish()]);
        }
        let submit_ns = t0.elapsed().as_nanos();
        if !readback || self.pending == 0 {
            self.vused = 0;
            self.pending = 0;
            return (submit_ns, 0);
        }
        let t1 = std::time::Instant::now();
        // target → readback buffer → map → композит в fb
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rlisp-readback"),
            });
        enc.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &self.readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(self.readback_pad as u32),
                    rows_per_image: Some(self.target_h as u32),
                },
            },
            wgpu::Extent3d {
                width: self.target_w as u32,
                height: self.target_h as u32,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |res| {
                let _ = tx.send(res);
            });
        self.device.poll(wgpu::Maintain::Wait);
        // callback кладёт Ok(()) или Err(BufferAsyncError); канал закрывается
        // только если поток умер — в обоих случаях fb не трогаем
        let mapped = match rx.recv() {
            Ok(r) => r,
            Err(_) => {
                eprintln!("[wgpu] readback: map-канал закрылся");
                self.vused = 0;
                self.pending = 0;
                return (submit_ns, t1.elapsed().as_nanos());
            }
        };
        if let Err(e) = mapped {
            // readback не удался — кадр отдаём как есть (fb не трогаем)
            eprintln!("[wgpu] readback: {}", e);
            self.vused = 0;
            self.pending = 0;
            return (submit_ns, t1.elapsed().as_nanos());
        }
        {
            let view = self.readback.slice(..).get_mapped_range();
            composite_readback(
                fb,
                w.min(self.target_w),
                h.min(self.target_h),
                self.readback_pad,
                &view,
            );
        }
        self.readback.unmap();
        let readback_ns = t1.elapsed().as_nanos();
        // сброс покрытия: следующий кадр начинает чистую alpha-маску
        self.clear_transparent();
        self.vused = 0;
        self.pending = 0;
        (submit_ns, readback_ns)
    }
}

#[cfg(feature = "wgpu")]
impl WgpuRenderer {
    /// alpha=0 везде: GPU-слой больше ничего не покрывает (следующий кадр)
    fn clear_transparent(&mut self) {
        let view = self
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rlisp-reset"),
            });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rlisp-reset-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit([enc.finish()]);
    }
}

// ————————————————————————————————————————————————————————————————————————
// Тесты: книга ресурсов и композит — без устройства (wgpu-путь проверяется
// книгой хэндлов; device-тесты требуют адаптера и в CI допускают отсутствие)
// ————————————————————————————————————————————————————————————————————————

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ресурсы_регистр_фри_счётчики() {
        let mut r = GpuResources::default();
        r.register(1, 16 * 16 * 4);
        r.register(2, 8 * 8 * 4);
        assert_eq!(r.live(), 2);
        assert_eq!(r.bytes(), 16 * 16 * 4 + 8 * 8 * 4);
        assert_eq!(r.stats.tex_live(), 2);
        assert!(r.free(1));
        assert!(!r.free(1)); // повторное освобождение — no-op
        assert_eq!(r.live(), 1);
        assert_eq!(r.stats.tex_live(), 1);
    }

    #[test]
    fn ресурсы_перезагрузка_того_же_id_растит_поколение() {
        let mut r = GpuResources::default();
        r.register(1, 64);
        assert_eq!(r.generation(1), 1);
        r.register(1, 64);
        assert_eq!(r.generation(1), 2);
        assert_eq!(r.live(), 1);
        assert_eq!(r.stats.tex_created, 2);
        assert_eq!(r.stats.tex_freed, 1); // перезагрузка заместила старое поколение
        assert!(r.free(1));
        assert_eq!(r.stats.tex_freed, 2);
        assert_eq!(r.stats.tex_live(), 0); // создано 2 — освобождено 2, утечки нет
    }

    #[test]
    fn ресурсы_инвалидация_hot_reload_кроме_persistent() {
        let mut r = GpuResources::default();
        for id in 1..=5 {
            r.register(id, 64);
        }
        let freed = r.invalidate_except(&[2]);
        assert_eq!(freed, 4);
        assert_eq!(r.live(), 1);
        assert_eq!(r.stats.reload_invalidated, 4);
        assert_eq!(r.stats.reload_cycles, 1);
    }

    #[test]
    fn стресс_100_циклов_создать_перезагрузить_без_роста() {
        let mut r = GpuResources::default();
        for cycle in 0..100 {
            // как game3d.lisp за reload: 5 процедурных текстур + 1 persistent id=1
            for id in 1..=5usize {
                r.register(id as i64, 16 * 16 * 4);
            }
            r.invalidate_except(&[1]);
            assert_eq!(r.live(), 1, "цикл {}: после reload жива одна", cycle);
            assert_eq!(r.bytes(), 16 * 16 * 4, "цикл {}: байт не растёт", cycle);
        }
        assert_eq!(r.stats.tex_created, 500);
        assert_eq!(r.stats.tex_freed, 500 - 1);
        r.free(1);
        assert_eq!(r.stats.tex_live(), 0);
    }

    #[test]
    fn композит_readback_по_альфе_покрытия() {
        // fb: всё синее; GPU покрывает 1 пиксель красным + 1 пиксель прозрачным
        let (w, h) = (2usize, 1usize);
        let pad = 256usize;
        let mut fb = vec![0u8; w * h * 4];
        for p in 0..w * h {
            fb[p * 4] = 0;
            fb[p * 4 + 1] = 0;
            fb[p * 4 + 2] = 200;
            fb[p * 4 + 3] = 255;
        }
        let mut data = vec![0u8; pad * h];
        data[0] = 255;
        data[1] = 10;
        data[2] = 20;
        data[3] = 255; // пиксель 0: красный, покрыт
        data[4] = 99;
        data[5] = 99;
        data[6] = 99;
        data[7] = 0; // пиксель 1: прозрачный — fb не трогаем
        composite_readback(&mut fb, w, h, pad, &data);
        assert_eq!(&fb[0..4], &[255, 10, 20, 255]);
        assert_eq!(&fb[4..8], &[0, 0, 200, 255]);
    }

    #[test]
    fn фолбэк_без_фичи_даёт_понятное_предупреждение() {
        // без фичи `wgpu` create_wgpu_backend всегда Err → откат на софт
        let probe = create_wgpu_backend(64, 64);
        match &probe {
            Ok(b) => {
                // на машине с адаптером и собранной фичей — тоже валидный исход
                assert!(!b.adapter_info().is_empty());
            }
            Err(e) => {
                assert!(fallback_warning(&probe).is_some());
                assert!(!e.is_empty());
            }
        }
    }

    #[test]
    fn collect_int_refs_достаёт_id_из_таблиц_списков() {
        use crate::interp::Value;
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut out = vec![];
        collect_int_refs(
            &Value::List(vec![
                Value::Int(3),
                Value::Table(Rc::new(RefCell::new(
                    [("tex".into(), Value::Int(7))].into_iter().collect(),
                ))),
                Value::Str("не число".into()),
            ]),
            &mut out,
            0,
        );
        out.sort();
        assert_eq!(out, vec![3, 7]);
    }
}
