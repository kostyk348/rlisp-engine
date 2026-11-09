#![allow(
    clippy::module_inception,
    clippy::redundant_closure,
    clippy::map_identity
)]

// scripts/parse_diff.rs — сверка старого и нового reader'а на произвольном вводе.
// Запуск: cargo run --features jit -- --bench-parse-test  (или как test)
#[cfg(test)]
mod parse_diff {
    use crate::interp::{parse_program_scalar, parse_program_simd};

    fn diff(src: &str) {
        let a = parse_program_scalar(src);
        let b = parse_program_simd(src);
        match (&a, &b) {
            (Ok(x), Ok(y)) => {
                let fx: Vec<String> = x.iter().map(|v| crate::interp::fmt_value(v)).collect();
                let fy: Vec<String> = y.iter().map(|v| crate::interp::fmt_value(v)).collect();
                if fx != fy {
                    panic!(
                        "РАСХОЖДЕНИЕ AST:\n  вход: {:?}\n  scalar: {:?}\n  simd:   {:?}",
                        src, fx, fy
                    );
                }
            }
            (Err(e1), Err(e2)) => {
                if e1 != e2 {
                    panic!(
                        "РАСХОЖДЕНИЕ ошибок:\n  вход: {:?}\n  scalar: {}\n  simd:   {}",
                        src, e1, e2
                    );
                }
            }
            (r1, r2) => panic!(
                "РАСХОЖДЕНИЕ Ok/Err:\n  вход: {:?}\n  scalar ok: {}\n  simd ok:   {}",
                src,
                r1.is_ok(),
                r2.is_ok()
            ),
        }
    }

    #[test]
    fn сверка_парсера_на_каверзных_входах() {
        let cases = [
            "(1 . 2)",
            "(define p (cons 1 2)) (set-car! p 10) (set-cdr! p (list 20 30)) p",
            "(set-car! p 10)",
            "(a . b)",
            "'(a b) `(x ,y ,@z)",
            "(let ((x 1) (y 2)) (+ x y))",
            "(if (< 1 2) 3 4)",
            "1.5 -2 3e2 #t #f",
            "(\"строка с \"кавычками\"\")",
            "(\"a\\nb\\t\\\\c\")",
            "; комментарий\n(+ 1 2) ; хвост\n",
            "(a;комментарий\nb)",
            "",
            "   ",
            "()",
            "(()())",
            "((((()))))",
            "[1 2]",
            "(1 2]",
            "(1 2",
            "(1 . 2 . 3)",
            "(. x)",
            "'",
            "(1 .",
            "\"незакрытая",
            "\"\\\"",
            "αβγ αβ",
            "λ(x) фоо+бар",
            "(a\u{00A0}b)",
            "a\u{2028}b",
            "(x\u{3000}y)",
            "\u{0000}a",
            "(+ 1 -2 3.5e-2)",
            "(0.5 . 0.25)",
            "`(1 ,@x)",
            "(quasiquote (1 (unquote-splicing x)))",
            "(a (b (c (d))))",
            "100000000000000000000",
            "(1..2)",
            ".5",
            "(.)",
        ];
        for c in cases {
            diff(c);
        }
    }

    #[test]
    fn сверка_парсера_на_реальном_корпусе() {
        // реальные .lisp из репозитория: AST старого и нового пути обязаны совпасть
        let dir = std::path::Path::new("assets/lisp");
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("lisp") {
                    let src = std::fs::read_to_string(&p).unwrap();
                    let a = parse_program_scalar(&src);
                    let b = parse_program_simd(&src);
                    match (&a, &b) {
                        (Ok(x), Ok(y)) => {
                            if x.len() != y.len() {
                                panic!("{}: {} vs {} форм", p.display(), x.len(), y.len());
                            }
                            for (i, (vx, vy)) in x.iter().zip(y.iter()).enumerate() {
                                let fx = crate::interp::fmt_value(vx);
                                let fy = crate::interp::fmt_value(vy);
                                if fx != fy {
                                    panic!(
                                        "{}: форма {}:\n  scalar: {}\n  simd:   {}",
                                        p.display(),
                                        i + 1,
                                        fx,
                                        fy
                                    );
                                }
                            }
                        }
                        (Err(e1), Err(e2)) => {
                            if e1 != e2 {
                                panic!("{}: ошибка scalar={} simd={}", p.display(), e1, e2);
                            }
                        }
                        (r1, r2) => panic!(
                            "{}: Ok/Err расходится: scalar_ok={} simd_ok={}",
                            p.display(),
                            r1.is_ok(),
                            r2.is_ok()
                        ),
                    }
                    count += 1;
                }
            }
        }
        assert!(count >= 10, "корпус не найден: {} файлов", count);
    }
}
