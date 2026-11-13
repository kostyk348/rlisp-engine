// src/editor/http.rs — минимальный HTTP/1.1 и разбор запроса.
// Ноль зависимостей: BufReader/Write по TcpStream, ручной разбор строки
// запроса, заголовков, query и тела. Сюда же — защита путей (safe_lisp_path):
// редактор имеет право трогать только assets/lisp внутри корня проекта.

use crate::json::Json;
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};

pub(crate) fn http_res(stream: &mut TcpStream, code: &str, ctype: &str, body: Vec<u8>) {
    let head = format!(
        "HTTP/1.1 {} \r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        code,
        ctype,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

pub(crate) fn http_json(stream: &mut TcpStream, j: &Json) {
    http_res(
        stream,
        "200 OK",
        "application/json; charset=utf-8",
        j.dump().into_bytes(),
    );
}

pub(crate) fn http_err(stream: &mut TcpStream, _code: &str, msg: &str) {
    http_json(stream, &Json::obj(vec![("error", Json::str(msg))]));
}

pub(crate) fn body_field(body: &str, field: &str) -> Option<String> {
    crate::json::parse(body)
        .ok()?
        .get(field)?
        .as_str()
        .map(|s| s.to_string())
}

/// Достаём "?a=b" из пути запроса.
pub(crate) fn query(raw: &str) -> BTreeMapS {
    let mut m = BTreeMapS::default();
    if let Some(qpos) = raw.find('?') {
        for pair in raw[qpos + 1..].split('&') {
            let mut kv = pair.splitn(2, '=');
            let k = kv.next().unwrap_or("").to_string();
            let v = kv.next().unwrap_or("").to_string();
            m.0.insert(k, url_decode(&v));
        }
    }
    m
}

#[derive(Default)]
pub(crate) struct BTreeMapS(std::collections::BTreeMap<String, String>);

impl BTreeMapS {
    pub(crate) fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|s| s.as_str())
    }
}

pub(crate) fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Корень проекта. Редактор запускается со скриптом `assets/lisp/*.lisp`,
/// поэтому его root — это `assets/lisp`, а не корень репозитория.
pub(crate) fn project_root(root: &Path) -> PathBuf {
    if root.ends_with("assets/lisp") {
        // "assets/lisp" → "" (пустой родитель) — это текущий каталог, т.е. "."
        match root.parent().and_then(|p| p.parent()) {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        }
    } else {
        root.to_path_buf()
    }
}

/// Путь к .lisp: принимаем и относительный к assets/lisp, и относительный
/// к корню проекта (`assets/lisp/x.lisp`), и абсолютный. Возвращает
/// канонизированный путь, только если он внутри assets/lisp и это .lisp.
pub(crate) fn resolve_lisp(root: &Path, p: &str) -> Option<PathBuf> {
    let lisp = if root.ends_with("assets/lisp") {
        root.to_path_buf()
    } else {
        root.join("assets/lisp")
    };
    safe_lisp_path(&lisp, p).or_else(|| safe_lisp_path(&project_root(root), p))
}

pub(crate) fn safe_lisp_path(root: &std::path::Path, p: &str) -> Option<std::path::PathBuf> {
    let path = root.join(p);
    let root_canon = std::fs::canonicalize(root).ok()?;
    // файла может ещё не быть (создание из редактора) — тогда канонизируем
    // родителя, а имя оставляем как есть
    let canon = match std::fs::canonicalize(&path) {
        Ok(c) => c,
        Err(_) => {
            let parent = std::fs::canonicalize(path.parent()?).ok()?;
            parent.join(path.file_name()?)
        }
    };
    if canon.starts_with(&root_canon) && canon.extension().map(|e| e == "lisp").unwrap_or(false) {
        Some(canon)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn корень_проекта_вычисляется_из_root_редактора() {
        assert_eq!(project_root(Path::new("assets/lisp")), PathBuf::from("."));
        assert_eq!(
            project_root(Path::new("/w/proj/assets/lisp")),
            PathBuf::from("/w/proj")
        );
        // если root не похож на assets/lisp — он и есть корень
        assert_eq!(
            project_root(Path::new("/w/other")),
            PathBuf::from("/w/other")
        );
    }

    #[test]
    fn resolve_lisp_принимает_оба_относительных_вида_и_абсолютный() {
        let root = Path::new("assets/lisp");
        let a = resolve_lisp(root, "core.lisp").expect("core.lisp от assets/lisp");
        let b = resolve_lisp(root, "assets/lisp/core.lisp").expect("core.lisp от корня проекта");
        let c = resolve_lisp(
            root,
            &format!(
                "{}/assets/lisp/core.lisp",
                std::env::current_dir().unwrap().display()
            ),
        )
        .expect("абсолютный путь");
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert!(a.ends_with("assets/lisp/core.lisp"));
    }

    #[test]
    fn resolve_lisp_запрещает_выход_из_assets_и_не_lisp() {
        let root = Path::new("assets/lisp");
        assert!(
            resolve_lisp(root, "../main.rs").is_none(),
            "выход вверх запрещён"
        );
        assert!(resolve_lisp(root, "../../README.md").is_none());
        assert!(resolve_lisp(root, "core.txt").is_none(), "только .lisp");
        assert!(resolve_lisp(root, "/etc/passwd").is_none());
        // создание в несуществующем каталоге — тоже нет
        assert!(resolve_lisp(root, "nope/deep/x.lisp").is_none());
    }

    #[test]
    fn resolve_lisp_разрешает_новый_файл_для_создания() {
        let root = Path::new("assets/lisp");
        let p = resolve_lisp(root, "_new_unit_test.lisp").expect("новый файл в assets/lisp");
        // файла ещё нет, но путь канонизирован по родителю
        assert!(!p.exists());
        assert!(p.ends_with("assets/lisp/_new_unit_test.lisp"));
    }
}
