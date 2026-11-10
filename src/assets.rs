//! src/assets.rs — Этап 18 «Менеджер ассетов»: именованный реестр с типами,
//! рефкаунтами и явной сборкой. Слои не знают друг про друга: значение —
//! любое Lisp-значение (f32-вектор меша, таблица описания, звуковой буфер),
//! тип — строковый тег для валидации при получении.
//!
//! Семантика:
//!   put!  — создать/заменить значение (refs сохраняются при замене);
//!   ref!  — +1 к счётчику (взял в работу);
//!   release! — −1 (отдал); до нуля — живёт;
//!   gc!   — освободить всё с refs == 0 (явная, детерминированная);
//!   gen   — поколение: имя остаётся, слот переиспользуется после gc.
//! HAL про ассеты не знает.

use crate::interp::Value as V;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub struct Asset {
    pub name: String,
    pub kind: String,
    pub value: V,
    pub refs: u32,
}

#[derive(Default)]
pub struct AssetStore {
    /// слоты: None = свободен; slot-ids стабильны до gc!
    entries: Vec<Option<Asset>>,
    free: Vec<usize>,
    index: HashMap<String, usize>,
}

impl AssetStore {
    pub fn new() -> Self {
        AssetStore::default()
    }

    /// создать/заменить; возвращает (слот, поколение). При замене refs
    /// сохраняются (значение обновилось — держатели живы).
    pub fn put(&mut self, name: &str, kind: &str, value: V) -> Result<(usize, u32), String> {
        if name.is_empty() || kind.is_empty() {
            return Err("asset: пустое имя или тип".into());
        }
        if let Some(&slot) = self.index.get(name) {
            let a = self.entries[slot].as_mut().ok_or("asset: битый слот")?;
            a.kind = kind.to_string();
            a.value = value;
            return Ok((slot, a.refs));
        }
        let slot = match self.free.pop() {
            Some(s) => s,
            None => {
                self.entries.push(None);
                self.entries.len() - 1
            }
        };
        self.index.insert(name.to_string(), slot);
        self.entries[slot] = Some(Asset {
            name: name.to_string(),
            kind: kind.to_string(),
            value,
            refs: 0,
        });
        Ok((slot, 0))
    }

    pub fn get(&self, name: &str) -> Option<V> {
        let slot = *self.index.get(name)?;
        Some(self.entries[slot].as_ref()?.value.clone())
    }

    /// получить значение с проверкой типа (мismatch — ошибка, а не тихий nil)
    pub fn get_typed(&self, name: &str, kind: &str) -> Result<Option<V>, String> {
        let Some(&slot) = self.index.get(name) else {
            return Ok(None);
        };
        let a = self.entries[slot].as_ref().ok_or("asset: битый слот")?;
        if a.kind != kind {
            return Err(format!("asset {}: тип {}, а не {}", name, a.kind, kind));
        }
        Ok(Some(a.value.clone()))
    }

    pub fn retain(&mut self, name: &str) -> Result<u32, String> {
        let slot = *self
            .index
            .get(name)
            .ok_or_else(|| format!("asset {}: не найден", name))?;
        let a = self.entries[slot].as_mut().ok_or("asset: битый слот")?;
        a.refs += 1;
        Ok(a.refs)
    }

    pub fn release(&mut self, name: &str) -> Result<u32, String> {
        let slot = *self
            .index
            .get(name)
            .ok_or_else(|| format!("asset {}: не найден", name))?;
        let a = self.entries[slot].as_mut().ok_or("asset: битый слот")?;
        if a.refs == 0 {
            return Err(format!("asset {}: release без ref", name));
        }
        a.refs -= 1;
        Ok(a.refs)
    }

    /// явная сборка: освободить всё с refs == 0; возвращает имена освобождённых
    pub fn gc(&mut self) -> Vec<String> {
        let mut freed = Vec::new();
        for s in 0..self.entries.len() {
            let keep = match &self.entries[s] {
                Some(a) => a.refs > 0,
                None => false,
            };
            if !keep {
                // в пул свободных — только реально освобождённый слот
                // (уже пустой слот не дублируется при повторных gc)
                if let Some(a) = self.entries[s].take() {
                    self.index.remove(&a.name);
                    freed.push(a.name);
                    self.free.push(s);
                }
            }
        }
        freed
    }

    pub fn refs_of(&self, name: &str) -> Option<u32> {
        let slot = *self.index.get(name)?;
        Some(self.entries[slot].as_ref()?.refs)
    }

    /// имена по возрастанию слотов (детерминизм)
    pub fn list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in 0..self.entries.len() {
            if let Some(a) = &self.entries[s] {
                out.push(a.name.clone());
            }
        }
        out
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let alive = self.entries.iter().filter(|e| e.is_some()).count();
        (alive, self.entries.len(), self.free.len())
    }
}

/// ——— нативы Lisp: (asset-*) — реестр в Vec<AssetStore>, id — индекс ———
pub fn register(env: crate::interp::Gc, store: Rc<RefCell<Vec<AssetStore>>>) {
    use crate::interp::{env_define, need_args as need, Gc, Value as V};

    fn def<F: Fn(&[V]) -> Result<V, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, V::Native(Rc::new(f), name.to_string()));
    }

    fn str_of(v: &V) -> Result<String, String> {
        match v {
            V::Str(s) => Ok(s.clone()),
            V::Sym(s) => Ok(s.clone()),
            _ => Err("asset: ожидалась строка".into()),
        }
    }

    fn store_of<'a>(
        store: &'a Rc<RefCell<Vec<AssetStore>>>,
        v: &V,
    ) -> Result<std::cell::RefMut<'a, AssetStore>, String> {
        use crate::interp::num_val;
        let id = num_val(v)? as usize;
        let mut ss = store.borrow_mut();
        if id >= ss.len() {
            ss.push(AssetStore::new());
        }
        Ok(std::cell::RefMut::map(ss, |ss| &mut ss[id]))
    }

    // (asset-put! st "имя" "тип" значение) → слот
    let st = store.clone();
    def(env, "asset-put!", move |args| {
        need(args, 4)?;
        let name = str_of(&args[1])?;
        let kind = str_of(&args[2])?;
        let (slot, _) = store_of(&st, &args[0])?.put(&name, &kind, args[3].clone())?;
        Ok(V::Int(slot as i64))
    });

    // (asset-get st "имя" [тип]) → значение | #f
    let st = store.clone();
    def(env, "asset-get", move |args| {
        need(args, 2)?;
        let name = str_of(&args[1])?;
        let kind = if args.len() >= 3 {
            Some(str_of(&args[2])?)
        } else {
            None
        };
        let v = match kind {
            Some(k) => store_of(&st, &args[0])?.get_typed(&name, &k)?,
            None => store_of(&st, &args[0])?.get(&name),
        };
        Ok(v.unwrap_or(V::Bool(false)))
    });

    let st = store.clone();
    def(env, "asset-ref!", move |args| {
        need(args, 2)?;
        let name = str_of(&args[1])?;
        Ok(V::Int(store_of(&st, &args[0])?.retain(&name)? as i64))
    });

    let st = store.clone();
    def(env, "asset-release!", move |args| {
        need(args, 2)?;
        let name = str_of(&args[1])?;
        Ok(V::Int(store_of(&st, &args[0])?.release(&name)? as i64))
    });

    let st = store.clone();
    def(env, "asset-gc!", move |args| {
        need(args, 1)?;
        let freed = store_of(&st, &args[0])?.gc();
        Ok(V::List(freed.into_iter().map(V::Str).collect()))
    });

    let st = store.clone();
    def(env, "asset-list", move |args| {
        need(args, 1)?;
        Ok(V::List(
            store_of(&st, &args[0])?
                .list()
                .into_iter()
                .map(V::Str)
                .collect(),
        ))
    });

    let st = store.clone();
    def(env, "asset-refs", move |args| {
        need(args, 2)?;
        let name = str_of(&args[1])?;
        Ok(V::Int(
            store_of(&st, &args[0])?.refs_of(&name).unwrap_or(0) as i64
        ))
    });

    // (asset-stats st) → (живые слоты свободно)
    let st = store.clone();
    def(env, "asset-stats", move |args| {
        need(args, 1)?;
        let (a, s, f) = store_of(&st, &args[0])?.stats();
        Ok(V::List(vec![
            V::Int(a as i64),
            V::Int(s as i64),
            V::Int(f as i64),
        ]))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Value as V;

    /// get с обязательным Str-значением (Value не реализует PartialEq/Debug)
    fn get_str(s: &AssetStore, name: &str) -> Option<String> {
        match s.get(name) {
            Some(V::Str(t)) => Some(t),
            Some(_) => panic!("{} не Str", name),
            None => None,
        }
    }

    #[test]
    fn put_get_replace() {
        let mut s = AssetStore::new();
        let (slot, refs) = s.put("cnut", "mesh", V::Str("meshdata".into())).unwrap();
        assert_eq!(slot, 0);
        assert_eq!(refs, 0);
        assert_eq!(get_str(&s, "cnut"), Some("meshdata".into()));
        // замена: refs сохраняются
        s.retain("cnut").unwrap();
        let (_, refs) = s.put("cnut", "mesh", V::Str("meshdata2".into())).unwrap();
        assert_eq!(refs, 1);
        assert_eq!(get_str(&s, "cnut"), Some("meshdata2".into()));
        // тот же слот, не новый
        let (slot2, _) = s.put("other", "tex", V::Int(1)).unwrap();
        assert_eq!(slot2, 1);
    }

    #[test]
    fn typed_get_validates_kind() {
        let mut s = AssetStore::new();
        s.put("wav", "sound", V::Str("pcm".into())).unwrap();
        assert!(s.get_typed("wav", "sound").is_ok());
        let err = match s.get_typed("wav", "mesh") {
            Err(e) => e,
            Ok(_) => panic!("ожидали ошибку типа"),
        };
        assert!(err.contains("тип"), "{}", err);
        assert!(s.get_typed("нет", "mesh").unwrap().is_none());
    }

    #[test]
    fn refcount_and_explicit_gc() {
        let mut s = AssetStore::new();
        s.put("a", "mesh", V::Str("A".into())).unwrap();
        s.put("b", "mesh", V::Str("B".into())).unwrap();
        s.retain("a").unwrap();
        s.retain("a").unwrap();
        assert_eq!(s.refs_of("a"), Some(2));
        assert_eq!(s.release("a").unwrap(), 1);
        // gc: b (refs 0) уходит, a (refs 1) живёт
        let freed = s.gc();
        assert_eq!(freed, vec!["b".to_string()]);
        assert_eq!(get_str(&s, "a"), Some("A".into()));
        assert_eq!(get_str(&s, "b"), None);
        // release до нуля → gc освобождает; release без ref — ошибка
        s.release("a").unwrap();
        assert!(s.release("a").is_err(), "release без ref — ошибка");
        let freed = s.gc();
        assert_eq!(freed, vec!["a".to_string()]);
        assert_eq!(s.stats(), (0, 2, 2), "обе сущности освобождены");
        assert!(s.release("a").is_err(), "release уже освобождённого");
    }

    #[test]
    fn slot_reuse_after_gc() {
        let mut s = AssetStore::new();
        s.put("x", "mesh", V::Str("X".into())).unwrap();
        let freed = s.gc();
        assert_eq!(freed, vec!["x".to_string()]);
        let (slot, _) = s.put("y", "mesh", V::Str("Y".into())).unwrap();
        assert_eq!(slot, 0, "слот переиспользован из пула");
        assert_eq!(s.list(), vec!["y".to_string()]);
        assert_eq!(get_str(&s, "x"), None);
    }

    #[test]
    fn list_order_and_empty_name_rejected() {
        let mut s = AssetStore::new();
        s.put("zeta", "mesh", V::Int(1)).unwrap();
        s.put("alpha", "tex", V::Int(2)).unwrap();
        assert_eq!(s.list(), vec!["zeta".to_string(), "alpha".to_string()]);
        assert!(s.put("", "mesh", V::Int(1)).is_err());
        assert!(s.put("n", "", V::Int(1)).is_err());
    }
}
