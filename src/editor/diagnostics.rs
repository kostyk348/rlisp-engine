// src/editor/diagnostics.rs — ошибки Lisp с координатами для консоли редактора.
// Парсер движка не хранит позиций (он гоняет chars ради скорости), поэтому
// координата восстанавливается здесь: по индексу формы (для ошибок исполнения)
// или по фрагменту из текста ошибки (для ошибок разбора).

/// Строка (1-based) i-й формы верхнего уровня (0-based), с учётом строк,
/// комментариев и экранирования в строках.
pub fn form_line(code: &str, form_index: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut in_comment = false;
    let mut line = 1usize;
    let mut seen = 0usize;
    let mut started = false;
    for ch in code.chars() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
                line += 1;
            }
            continue;
        }
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            } else if ch == '\n' {
                line += 1;
            }
            continue;
        }
        match ch {
            ';' => in_comment = true,
            '"' => in_str = true,
            '(' | '[' => {
                if depth == 0 && !started {
                    if seen == form_index {
                        return Some(line);
                    }
                    seen += 1;
                    started = true;
                }
                depth += 1;
            }
            ')' | ']' => {
                depth -= 1;
                if depth <= 0 {
                    depth = 0;
                    started = false;
                }
            }
            '\n' => line += 1,
            // атом верхнего уровня — тоже форма
            c if !c.is_whitespace() && depth == 0 && !started => {
                if seen == form_index {
                    return Some(line);
                }
                seen += 1;
                started = true;
            }
            _ => {}
        }
    }
    None
}

/// Позиция фрагмента из текста ошибки: «…», '…' или кавычки-лапки. Возвращает
/// (строка 1-based, столбец 1-based, текст строки) — «возможно, здесь».
pub fn locate_fragment(code: &str, err: &str) -> Option<(usize, usize, String)> {
    let mut frags: Vec<String> = Vec::new();
    for (open, close) in [('«', '»'), ('\'', '\''), ('`', '`')] {
        let mut rest = err;
        while let Some(a) = rest.find(open) {
            let tail = &rest[a + open.len_utf8()..];
            match tail.find(close) {
                Some(b) if b > 0 => {
                    let f = &tail[..b];
                    if f.chars().count() <= 40 {
                        frags.push(f.to_string());
                    }
                    rest = &tail[b + close.len_utf8()..];
                }
                _ => break,
            }
        }
    }
    for frag in frags {
        if let Some(pos) = code.find(&frag) {
            let before = &code[..pos];
            let line = before.chars().filter(|c| *c == '\n').count() + 1;
            let col = before
                .rsplit('\n')
                .next()
                .map(|s| s.chars().count() + 1)
                .unwrap_or(1);
            let text = code.lines().nth(line - 1).unwrap_or("").to_string();
            return Some((line, col, text));
        }
    }
    None
}

/// Дополнить сообщение об ошибке координатой: «(форма #2, строка 7)».
pub fn annotate(code: &str, form_index: Option<usize>, err: &str) -> String {
    if let Some(i) = form_index {
        if let Some(line) = form_line(code, i) {
            return format!("{} (форма #{}, строка {})", err, i + 1, line);
        }
    }
    if let Some((line, col, text)) = locate_fragment(code, err) {
        let trimmed = text.trim_end();
        return format!(
            "{} (возможно, строка {}, столбец {}: `{}`)",
            err, line, col, trimmed
        );
    }
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn строка_формы_считается_по_тексту() {
        let code = "(a 1)\n\n  ; комментарий (не форма)\n(b (c 2))\n(d)";
        assert_eq!(form_line(code, 0), Some(1));
        assert_eq!(form_line(code, 1), Some(4));
        assert_eq!(form_line(code, 2), Some(5));
        assert_eq!(form_line(code, 9), None);
    }

    #[test]
    fn строка_внутри_строкового_литерала_не_считается() {
        // перевод строки ВНУТРИ литерала — физическая строка, значит (b) на 3-й,
        // а форма по-прежнему вторая (скобки внутри литерала не считаются)
        let code = "(a \"x\n(%s)\" 1)\n(b)";
        assert_eq!(form_line(code, 1), Some(3));
        let plain = "(a \"(%s)\" 1)\n(b)";
        assert_eq!(form_line(plain, 1), Some(2));
    }

    #[test]
    fn фрагмент_из_ошибки_даёт_координату() {
        let code = "(foo 1)\n(bar «bad» 2)";
        let (line, _col, text) = locate_fragment(code, "неизвестно: «bad»").unwrap();
        assert_eq!(line, 2);
        assert!(text.contains("bad"));
    }

    #[test]
    fn аннотация_использует_форму_а_не_фрагмент() {
        let code = "(ok)\n(bad 2)";
        let msg = annotate(code, Some(1), "не число: bad");
        assert!(msg.contains("форма #2"), "{}", msg);
        assert!(msg.contains("строка 2"), "{}", msg);
    }
}
