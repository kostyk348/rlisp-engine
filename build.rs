// build.rs — детект компилятора без зависимостей.
//
// Интринсики AVX-512 (_mm512_*) в stable rustc стабилизированы в 1.89
// (stdarch_x86_avx512). Модуль simd::avx512 компилируется только там, где
// компилятор умеет: cfg has_avx512 выставляется здесь, если rustc >= 1.89
// (или nightly, где фича доступна). На старых stable (например 1.83) модуль
// исключается, диспетчеризация честно выбирает AVX2/SSE — см. simd.rs.
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_avx512)");
    println!("cargo:rerun-if-changed=build.rs");
    let Some(out) = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    else {
        return; // не смогли спросить — консервативно не включаем avx512
    };
    // форматы: «rustc 1.89.0 (…») / «rustc 1.83.0-nightly (…») / «rustc 1.90.0-beta …»
    let nightly = out.contains("nightly") || out.contains("-dev");
    let Some((_, ver)) = out.split_once(' ') else {
        return;
    };
    let minor: u32 = match ver.split('.').nth(1).and_then(|m| m.trim().parse().ok()) {
        Some(m) => m,
        None => return,
    };
    // 1.89 — стабилизация AVX-512 stdarch; nightly и так умеет
    if nightly || minor >= 89 {
        println!("cargo:rustc-cfg=has_avx512");
    }
}
