// src/jit/emit.rs — эмиссия x86-64 машинного кода из линейного IR (copy-and-patch).
//
// Байтовые шаблоны инструкций собраны вручную (zero-dep):
//   movsd xmm0,[rip+d32]     F2 0F 10 05 d32        — константа из пула
//   movsd xmm0,[rdi+d8]      F2 0F 10 44 C7 d8      — параметр k (k*8 ≤ 127)
//   sub rsp,8 / add rsp,8    48 83 EC 08 / 48 83 C4 08
//   movsd [rsp],xmm0         F2 0F 11 04 24
//   movsd xmm1,[rsp]         F2 0F 10 0C 24
//   movsd xmm0,[rsp+d8]      F2 0F 10 44 24 d8      — let-слот
//   addsd xmm0,xmm1          F2 0F 58 C1
//   subsd xmm1,xmm0          F2 0F 5C C8  (+ movapd xmm0,xmm1 66 0F 28 C1)
//   mulsd xmm0,xmm1          F2 0F 59 C1
//   divsd xmm1,xmm0          F2 0F 5E C8  (+ movapd)
//   comisd xmm1,xmm0         66 0F 2F C8            — флаги: левый ? правый
//   jp rel32                 0F 8A d32              — NaN/unordered → false-путь
//   jcc rel32                0F cc d32              — код условия
//   jmp rel32                E9 d32
//   setcc al                 0F cc C0               — булев результат сравнения
//   xorpd xmm0,xmm0          66 0F EF C0            — 0.0 = #f
//   movd xmm0,eax            66 0F 6E C0            — {0,1} → xmm
//   cvtsi2sd xmm0,xmm0       F2 0F 2A C0            — int → double
//
// Переходы и адреса констант патчатся rel32 после эмиссии (assembler-fixups).
// Память: mmap(RW) → запись → mprotect(R|X) — W^X, потом munmap в Drop.

use super::{IrOp, JitFn};

enum Fix {
    /// rel32 к IR-цели: (patch_at, ir_index)
    Target(usize, usize),
    /// rel32 к константе пула: (patch_at, const_idx)
    Const(usize, usize),
    /// сырой фиксап внутри шаблона: (patch_at, abs_offset_в_коде)
    Raw(usize, usize),
}

struct Emitter {
    code: Vec<u8>,
    fixups: Vec<Fix>,
    labels: Vec<usize>,
    /// текущая глубина f64-стека (для LoadSlot)
    sp: usize,
}

impl Emitter {
    fn new() -> Self {
        Emitter {
            code: Vec::new(),
            fixups: Vec::new(),
            labels: Vec::new(),
            sp: 0,
        }
    }

    fn emit(&mut self, b: &[u8]) -> usize {
        let off = self.code.len();
        self.code.extend_from_slice(b);
        off
    }

    fn emit_u32(&mut self, v: u32) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    fn mark_label(&mut self, ir: usize) {
        if ir >= self.labels.len() {
            self.labels.resize(ir + 1, usize::MAX);
        }
        self.labels[ir] = self.code.len();
    }

    fn op_const(&mut self, idx: usize) {
        let off = self.emit(&[0xF2, 0x0F, 0x10, 0x05]); // movsd xmm0,[rip+d32]
        let disp = off + 4; // поле disp32 — фиксап именно туда
        self.fixups.push(Fix::Const(disp, idx));
        self.emit_u32(0);
    }

    /// |x|: маска грузится через movsd в xmm1 (movsd — unaligned-safe;
    /// прямой andpd с памятью требует выравнивания 16 и даёт #GP), затем
    /// регистровый andpd: старшие 64 бита обнулены movsd — для double безвредно
    fn op_abs(&mut self, idx: usize) {
        let off = self.emit(&[0xF2, 0x0F, 0x10, 0x0D]); // movsd xmm1,[rip+d32]
        let disp = off + 4;
        self.fixups.push(Fix::Const(disp, idx));
        self.emit_u32(0);
        self.emit(&[0x66, 0x0F, 0x54, 0xC1]); // andpd xmm0, xmm1
    }

    fn op_arg(&mut self, k: usize) {
        let d = k * 8;
        if d <= 127 {
            // movsd xmm0,[rdi+d8]: modrm 0x47 (mod=01, reg=xmm0, rm=rdi)
            self.emit(&[0xF2, 0x0F, 0x10, 0x47, d as u8]);
        } else {
            // modrm 0x87 (mod=10, reg=xmm0, rm=rdi) + disp32
            self.emit(&[0xF2, 0x0F, 0x10, 0x87]);
            self.emit_u32(d as u32);
        }
    }

    fn op_push(&mut self) {
        self.emit(&[0x48, 0x83, 0xEC, 0x08]); // sub rsp,8
        self.emit(&[0xF2, 0x0F, 0x11, 0x04, 0x24]); // movsd [rsp],xmm0
        self.sp += 1;
    }

    fn op_pop(&mut self) {
        self.emit(&[0xF2, 0x0F, 0x10, 0x0C, 0x24]); // movsd xmm1,[rsp]
        self.emit(&[0x48, 0x83, 0xC4, 0x08]); // add rsp,8
        self.sp -= 1;
    }

    /// true → слот вне окна (деоптимизация)
    fn op_load_slot(&mut self, id: usize) -> bool {
        if id >= self.sp {
            return true;
        }
        let off = (self.sp - 1 - id) * 8;
        if off <= 127 {
            self.emit(&[0xF2, 0x0F, 0x10, 0x44, 0x24, off as u8]); // movsd xmm0,[rsp+d8]
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x84, 0x24]);
            self.emit_u32(off as u32);
        }
        false
    }

    fn op_adjust(&mut self, k: usize) {
        let d = k * 8;
        if d <= 127 {
            self.emit(&[0x48, 0x83, 0xC4, d as u8]); // add rsp, d8
        } else {
            self.emit(&[0x48, 0x81, 0xC4]); // add rsp, d32
            self.emit_u32(d as u32);
        }
        self.sp -= k;
    }

    fn op_bin(&mut self, op: &IrOp, nan_idx: usize, end_ir: usize) {
        match op {
            IrOp::BinAdd => {
                self.emit(&[0xF2, 0x0F, 0x58, 0xC1]); // addsd xmm0,xmm1
            }
            IrOp::BinMul => {
                self.emit(&[0xF2, 0x0F, 0x59, 0xC1]); // mulsd xmm0,xmm1
            }
            IrOp::BinSub | IrOp::BinDiv => {
                let sub = matches!(op, IrOp::BinSub);
                if !sub {
                    // ГАРД ДЕОПТИМИЗАЦИИ: интерпретатор ОШИБКУЕТ на делении на
                    // ноль, f64-деление даёт ±inf/NaN. Делитель == ±0.0 →
                    // протокол деоптимизации: вернуть NaN, Rust-обёртка
                    // увидит NaN и откатится в интерпретатор (он вернёт
                    // честную ошибку «деление на ноль»).
                    self.emit(&[0x66, 0x0F, 0xEF, 0xD2]); // xorpd xmm2,xmm2
                    self.emit(&[0x66, 0x0F, 0x2F, 0xC2]); // comisd xmm0,xmm2
                    let jne_at = self.emit(&[0x0F, 0x85]); // jne → само деление
                    self.emit_u32(0);
                    // деоптимизация: восстановить rsp (могут быть висеть
                    // операнды-стек), NaN-сигнал и выход на ret
                    if self.sp > 0 {
                        let d = self.sp * 8;
                        if d <= 127 {
                            self.emit(&[0x48, 0x83, 0xC4, d as u8]); // add rsp, d8
                        } else {
                            self.emit(&[0x48, 0x81, 0xC4]); // add rsp, d32
                            self.emit_u32(d as u32);
                        }
                    }
                    self.emit(&[0xF2, 0x0F, 0x10, 0x05]); // movsd xmm0,[rip+nan]
                    let nan_patch = self.code.len();
                    self.emit_u32(0);
                    let jmp_at = self.emit(&[0xE9]); // jmp → ret
                    self.emit_u32(0);
                    let div_here = self.code.len();
                    self.fixups.push(Fix::Raw(jne_at + 2, div_here));
                    self.fixups.push(Fix::Const(nan_patch, nan_idx));
                    self.fixups.push(Fix::Target(jmp_at + 1, end_ir));
                }
                self.emit(&[0xF2, 0x0F, if sub { 0x5C } else { 0x5E }, 0xC8]);
                self.emit(&[0x66, 0x0F, 0x28, 0xC1]); // movapd xmm0,xmm1
            }
            _ => unreachable!(),
        }
    }

    fn op_cmp(&mut self, op: &IrOp) {
        // comisd xmm1,xmm0: флаги (левый ? правый)
        self.emit(&[0x66, 0x0F, 0x2F, 0xC8]);
        let jp_at = self.emit(&[0x0F, 0x8A]); // jp → false (NaN/unordered)
        self.emit_u32(0);
        let setcc: u8 = match op {
            // ВАЖНО: comisd задаёт только CF/ZF/PF — SF/OF не определены.
            // Поэтому «меньше» — setb (CF), «больше» — seta (¬CF ∧ ¬ZF),
            // «≤» — setbe (CF ∨ ZF), «≥» — setae (¬CF). NaN отсеян jp выше.
            IrOp::CmpLt => 0x92, // setb  (CF=1)
            IrOp::CmpEq => 0x94, // sete  (ZF=1)
            IrOp::CmpGt => 0x97, // seta  (CF=0 ∧ ZF=0)
            IrOp::CmpLe => 0x96, // setbe (CF=1 ∨ ZF=1)
            IrOp::CmpGe => 0x93, // setae (CF=0)
            _ => unreachable!(),
        };
        // булево из флагов в xmm0: setcc al → movzx eax,al → movd xmm0,eax
        // → cvtsi2sd xmm0,xmm0 (0.0/1.0)
        self.emit(&[0x0F, setcc, 0xC0]);
        self.emit(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
        self.emit(&[0x66, 0x0F, 0x6E, 0xC0]); // movd xmm0, eax
        self.emit(&[0xF2, 0x0F, 0x2A, 0xC0]); // cvtsi2sd xmm0, xmm0
        let jmp_at = self.emit(&[0xE9]); // jmp end
        self.emit_u32(0);
        let false_here = self.code.len();
        self.emit(&[0x66, 0x0F, 0xEF, 0xC0]); // xorpd xmm0,xmm0 → 0.0 (#f)
        let end_here = self.code.len();
        self.fixups.push(Fix::Raw(jp_at + 2, false_here));
        self.fixups.push(Fix::Raw(jmp_at + 1, end_here));
    }

    fn op_branch(&mut self, cond: u8, then_ir: usize, else_ir: usize) {
        // cond уже вычислен (comisd-флаги от (cmp a b)): NaN → else, cc → then
        self.emit(&[0x66, 0x0F, 0x2F, 0xC8]);
        let jp_at = self.emit(&[0x0F, 0x8A]);
        self.emit_u32(0);
        let jcc_at = self.emit(&[0x0F, cond]);
        self.emit_u32(0);
        self.fixups.push(Fix::Target(jp_at + 2, else_ir)); // fallthrough — тоже else
        self.fixups.push(Fix::Target(jcc_at + 2, then_ir));
    }

    fn op_jump(&mut self, ir: usize) {
        let at = self.emit(&[0xE9]);
        self.emit_u32(0);
        self.fixups.push(Fix::Target(at + 1, ir));
    }

    fn finish(mut self, consts: &[f64], arity: usize) -> Option<JitFn> {
        // пул констант (выровнен на 8)
        while self.code.len() % 8 != 0 {
            self.code.push(0xCC);
        }
        let pool_off = self.code.len();
        for c in consts {
            self.code.extend_from_slice(&c.to_le_bytes());
        }
        let total = self.code.len();
        // патчи rel32 = value - (адрес следующей инструкции)
        for f in &self.fixups {
            let (at, value) = match *f {
                Fix::Target(at, ir_idx) => {
                    let target = self.labels.get(ir_idx).copied().unwrap_or(usize::MAX);
                    if target == usize::MAX || target > total {
                        return None;
                    }
                    (at, target as i64)
                }
                Fix::Const(at, idx) => (at, (pool_off + idx * 8) as i64),
                Fix::Raw(at, abs) => (at, abs as i64),
            };
            let rel = value - ((at + 4) as i64);
            if rel > i32::MAX as i64 || rel < i32::MIN as i64 {
                return None;
            }
            let b = (rel as i32).to_le_bytes();
            self.code[at..at + 4].copy_from_slice(&b);
        }
        let addr = alloc_code(total)?;
        unsafe {
            std::ptr::copy_nonoverlapping(self.code.as_ptr(), addr as *mut u8, total);
        }
        make_exec(addr, total)?;
        Some(JitFn {
            code: addr,
            len: total,
            arity,
            consts: consts.to_vec(),
        })
    }
}

/// Главная функция эмиссии: IR + константы → исполняемый код.
/// В пул добавляется NaN-константа — сигнал runtime-деоптимизации
/// (деление на ноль: интерпретатор в этом месте возвращает ошибку).
pub fn emit(ir: &[IrOp], consts: &[f64], arity: usize) -> Option<JitFn> {
    let mut all_consts: Vec<f64> = consts.to_vec();
    let nan_idx = all_consts.len();
    all_consts.push(f64::NAN);
    let abs_idx = all_consts.len();
    // маска |x|: 0x7FFF…FFFF (знаковый бит снят andpd)
    all_consts.push(f64::from_bits(0x7FFF_FFFF_FFFF_FFFF));
    let end_ir = ir.len();
    let mut e = Emitter::new();
    for (i, op) in ir.iter().enumerate() {
        e.mark_label(i);
        match op {
            IrOp::Const(idx) => e.op_const(*idx),
            IrOp::Arg(k) => e.op_arg(*k),
            IrOp::Push => e.op_push(),
            IrOp::Pop => e.op_pop(),
            IrOp::LoadSlot(id) => {
                if e.op_load_slot(*id) {
                    return None;
                }
            }
            IrOp::AdjustStack(k) => e.op_adjust(*k),
            IrOp::BinAdd | IrOp::BinSub | IrOp::BinMul | IrOp::BinDiv => {
                e.op_bin(op, nan_idx, end_ir)
            }
            IrOp::CmpLt | IrOp::CmpEq | IrOp::CmpGt | IrOp::CmpLe | IrOp::CmpGe => e.op_cmp(op),
            IrOp::UnFloor => {
                e.emit(&[0x66, 0x0F, 0x3A, 0x0B, 0xC0, 0x01]); // roundsd imm=1
            }
            IrOp::UnSqrt => {
                e.emit(&[0xF2, 0x0F, 0x51, 0xC0]); // sqrtsd
            }
            IrOp::UnAbs => e.op_abs(abs_idx),
            IrOp::Min => {
                e.emit(&[0xF2, 0x0F, 0x5D, 0xC1]); // minsd xmm0, xmm1
            }
            IrOp::Max => {
                e.emit(&[0xF2, 0x0F, 0x5F, 0xC1]); // maxsd
            }
            IrOp::Branch {
                cond,
                then_ir,
                else_ir,
                end_ir: _,
            } => {
                e.op_branch(*cond, *then_ir, *else_ir);
            }
            IrOp::Jump(t) => e.op_jump(*t),
            IrOp::Placeholder => return None, // не должен дожить до эмиссии
        }
    }
    // виртуальный конец IR: метка на `ret` (целевые переходы end_ir = ir.len())
    e.mark_label(ir.len());
    e.emit(&[0xC3]); // ret
    e.finish(&all_consts, arity)
}

// ——————————————————————————— исполняемая память ———————————————————————————
// Прямые Linux-сисвызвы (zero-dep): mmap(2), mprotect(2), munmap(2).

#[cfg(target_os = "linux")]
const SYS_MMAP: i64 = 9;
#[cfg(target_os = "linux")]
const SYS_MPROTECT: i64 = 10;
#[cfg(target_os = "linux")]
const SYS_MUNMAP: i64 = 11;
const PROT_READ_WRITE: i64 = 0x3;
const PROT_READ_EXEC: i64 = 0x5;
const MAP_PRIVATE_ANON: i64 = 0x02 | 0x20;

/// mmap(NULL, len, RW, MAP_PRIVATE|ANON) → адрес или None.
fn alloc_code(len: usize) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    unsafe {
        let ret: i64;
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_MMAP => ret,
            in("rdi") 0usize,           // addr: пусть ядро выберет
            in("rsi") len,
            in("rdx") PROT_READ_WRITE,
            in("r10") MAP_PRIVATE_ANON,
            in("r8") (-1i64),           // fd
            in("r9") 0i64,              // offset
            lateout("rcx") _, lateout("r11") _,
            options(nostack)
        );
        if ret < 0 {
            return None;
        }
        return Some(ret as usize);
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    {
        let _ = len;
        None
    }
}

/// mprotect(addr, len, R|X)
fn make_exec(addr: usize, len: usize) -> Option<()> {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    unsafe {
        let ret: i64;
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_MPROTECT => ret,
            in("rdi") addr,
            in("rsi") len,
            in("rdx") PROT_READ_EXEC,
            lateout("rcx") _, lateout("r11") _,
            options(nostack)
        );
        if ret != 0 {
            return None;
        }
        return Some(());
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    {
        let _ = (addr, len);
        None
    }
}

pub fn release_code(addr: usize, len: usize) {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    unsafe {
        let _ret: i64;
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_MUNMAP => _ret,
            in("rdi") addr,
            in("rsi") len,
            lateout("rcx") _, lateout("r11") _,
            options(nostack)
        );
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    {
        let _ = (addr, len);
    }
}
