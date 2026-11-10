//! src/ecs.rs — Этап 13 «ECS в ядре»: сущности со слотовым хранением и
//! поколениями (generation handles). Lisp-уровень (assets/lisp/ecs.lisp,
//! таблицы) остаётся как DSL; натив даёт O(1) доступ по слоту,
//! переиспользование слотов со страховкой от «зомби»-хендлов и запросы
//! по набору компонентов без перебора пользовательских таблиц.
//! HAL про ECS не знает; значения компонентов — любые Lisp-значения.

use crate::interp::Value as V;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
pub struct Ecs {
    alive: Vec<bool>,
    gen: Vec<u32>,
    free: Vec<u32>,
    /// [слот][id компонента] — инвариант: длина строки == names.len()
    comps: Vec<Vec<Option<V>>>,
    names: Vec<String>,
    alive_n: usize,
}

/// хендл = (поколение << 32) | слот: после kill слот переиспользуется с
/// новым поколением — старый хендл становится недействительным
#[inline]
fn pack(slot: usize, gen: u32) -> u64 {
    ((gen as u64) << 32) | slot as u64
}

impl Ecs {
    pub fn new() -> Self {
        Ecs::default()
    }

    pub fn make(&mut self) -> u64 {
        let slot = match self.free.pop() {
            Some(s) => s as usize,
            None => {
                self.alive.push(false);
                self.gen.push(0);
                self.comps.push(vec![None; self.names.len()]);
                self.alive.len() - 1
            }
        };
        self.alive[slot] = true;
        self.alive_n += 1;
        pack(slot, self.gen[slot])
    }

    fn slot_of(&self, h: u64) -> Option<usize> {
        let slot = (h & 0xFFFF_FFFF) as usize;
        let g = (h >> 32) as u32;
        if slot < self.alive.len() && self.alive[slot] && self.gen[slot] == g {
            Some(slot)
        } else {
            None
        }
    }

    pub fn is_alive(&self, h: u64) -> bool {
        self.slot_of(h).is_some()
    }

    pub fn kill(&mut self, h: u64) -> bool {
        let Some(s) = self.slot_of(h) else {
            return false;
        };
        self.alive[s] = false;
        self.gen[s] = self.gen[s].wrapping_add(1);
        for c in self.comps[s].iter_mut() {
            *c = None;
        }
        self.free.push(s as u32);
        self.alive_n -= 1;
        true
    }

    fn comp_id(&mut self, name: &str) -> usize {
        if let Some(i) = self.names.iter().position(|n| n == name) {
            return i;
        }
        self.names.push(name.to_string());
        let id = self.names.len() - 1;
        for row in self.comps.iter_mut() {
            row.push(None);
        }
        id
    }

    pub fn set(&mut self, h: u64, name: &str, v: V) -> Result<(), String> {
        let s = self.slot_of(h).ok_or("ecs-set!: сущность мертва")?;
        let id = self.comp_id(name);
        self.comps[s][id] = Some(v);
        Ok(())
    }

    fn name_id(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    pub fn get(&self, h: u64, name: &str) -> Option<V> {
        let s = self.slot_of(h)?;
        let id = self.name_id(name)?;
        self.comps[s][id].clone()
    }

    pub fn has(&self, h: u64, name: &str) -> bool {
        match (self.slot_of(h), self.name_id(name)) {
            (Some(s), Some(id)) => self.comps[s][id].is_some(),
            _ => false,
        }
    }

    pub fn del(&mut self, h: u64, name: &str) -> bool {
        let Some(s) = self.slot_of(h) else {
            return false;
        };
        let Some(id) = self.name_id(name) else {
            return false;
        };
        self.comps[s][id].take().is_some()
    }

    /// хендлы сущностей, у которых есть ВСЕ перечисленные компоненты,
    /// по возрастанию слотов (детерминизм); неизвестные имена → пусто
    pub fn query(&self, names: &[&str]) -> Vec<u64> {
        let mut ids = Vec::with_capacity(names.len());
        for n in names {
            match self.name_id(n) {
                Some(i) => ids.push(i),
                None => return Vec::new(),
            }
        }
        let mut out = Vec::new();
        for s in 0..self.alive.len() {
            if self.alive[s] && ids.iter().all(|&id| self.comps[s][id].is_some()) {
                out.push(pack(s, self.gen[s]));
            }
        }
        out
    }

    pub fn count(&self) -> usize {
        self.alive_n
    }

    /// имена компонентов живой сущности (для интроспекции)
    pub fn comps_of(&self, h: u64) -> Vec<String> {
        let Some(s) = self.slot_of(h) else {
            return Vec::new();
        };
        (0..self.names.len())
            .filter(|&id| self.comps[s][id].is_some())
            .map(|id| self.names[id].clone())
            .collect()
    }
}

/// ——— нативы Lisp: (ecs-*) — реестр в Vec<Ecs>, id — индекс ———
pub fn register(env: crate::interp::Gc, store: Rc<RefCell<Vec<Ecs>>>) {
    use crate::interp::{env_define, need_args as need, num_val, Gc, Value as V};

    fn def<F: Fn(&[V]) -> Result<V, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, V::Native(Rc::new(f), name.to_string()));
    }

    fn str_of(v: &V) -> Result<String, String> {
        match v {
            V::Str(s) => Ok(s.clone()),
            V::Sym(s) => Ok(s.clone()),
            _ => Err("ecs: ожидалась строка (имя компонента)".into()),
        }
    }

    fn ecs_of<'a>(
        store: &'a Rc<RefCell<Vec<Ecs>>>,
        v: &V,
    ) -> Result<std::cell::RefMut<'a, Ecs>, String> {
        let id = num_val(v)? as usize;
        let mut ss = store.borrow_mut();
        if id >= ss.len() {
            ss.push(Ecs::new());
        }
        Ok(std::cell::RefMut::map(ss, |ss| &mut ss[id]))
    }

    fn handle_of(v: &V) -> Result<u64, String> {
        match v {
            V::Int(i) if *i >= 0 => Ok(*i as u64),
            _ => Err("ecs: ожидался хендл (целое ≥ 0)".into()),
        }
    }

    let st = store.clone();
    def(env, "ecs-new", move |args| {
        need(args, 0)?;
        let mut ss = st.borrow_mut();
        ss.push(Ecs::new());
        Ok(V::Int((ss.len() - 1) as i64))
    });

    let st = store.clone();
    def(env, "ecs-make!", move |args| {
        need(args, 1)?;
        let h = ecs_of(&st, &args[0])?.make();
        Ok(V::Int(h as i64))
    });

    let st = store.clone();
    def(env, "ecs-kill!", move |args| {
        need(args, 2)?;
        let h = handle_of(&args[1])?;
        Ok(V::Bool(ecs_of(&st, &args[0])?.kill(h)))
    });

    let st = store.clone();
    def(env, "ecs-alive?", move |args| {
        need(args, 2)?;
        let h = handle_of(&args[1])?;
        Ok(V::Bool(ecs_of(&st, &args[0])?.is_alive(h)))
    });

    let st = store.clone();
    def(env, "ecs-set!", move |args| {
        need(args, 4)?;
        let h = handle_of(&args[1])?;
        let name = str_of(&args[2])?;
        ecs_of(&st, &args[0])?
            .set(h, &name, args[3].clone())
            .map(|_| V::Nil)
    });

    let st = store.clone();
    def(env, "ecs-get", move |args| {
        need(args, 3)?;
        let h = handle_of(&args[1])?;
        let name = str_of(&args[2])?;
        Ok(ecs_of(&st, &args[0])?
            .get(h, &name)
            .unwrap_or(V::Bool(false)))
    });

    let st = store.clone();
    def(env, "ecs-has?", move |args| {
        need(args, 3)?;
        let h = handle_of(&args[1])?;
        let name = str_of(&args[2])?;
        Ok(V::Bool(ecs_of(&st, &args[0])?.has(h, &name)))
    });

    let st = store.clone();
    def(env, "ecs-del!", move |args| {
        need(args, 3)?;
        let h = handle_of(&args[1])?;
        let name = str_of(&args[2])?;
        Ok(V::Bool(ecs_of(&st, &args[0])?.del(h, &name)))
    });

    // (ecs-query e "a" "b" ...) — сущности со ВСЕМИ компонентами, по слотам
    let st = store.clone();
    def(env, "ecs-query", move |args| {
        need(args, 2)?;
        let names: Vec<String> = args[1..].iter().map(str_of).collect::<Result<_, _>>()?;
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let hs = ecs_of(&st, &args[0])?.query(&refs);
        Ok(V::List(hs.into_iter().map(|h| V::Int(h as i64)).collect()))
    });

    let st = store.clone();
    def(env, "ecs-count", move |args| {
        need(args, 1)?;
        Ok(V::Int(ecs_of(&st, &args[0])?.count() as i64))
    });

    let st = store.clone();
    def(env, "ecs-comps", move |args| {
        need(args, 2)?;
        let h = handle_of(&args[1])?;
        let names = ecs_of(&st, &args[0])?.comps_of(h);
        Ok(V::List(names.into_iter().map(V::Str).collect()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Value as V;

    /// get с обязательным Int (Value не реализует PartialEq/Debug)
    fn get_int(e: &Ecs, h: u64, name: &str) -> Option<i64> {
        match e.get(h, name) {
            Some(V::Int(i)) => Some(i),
            Some(_) => panic!("компонент {} не Int", name),
            None => None,
        }
    }

    #[test]
    fn make_set_get_lifecycle() {
        let mut e = Ecs::new();
        let h = e.make();
        assert!(e.is_alive(h));
        e.set(h, "hp", V::Int(100)).unwrap();
        e.set(h, "pos", V::List(vec![V::Float(1.5), V::Float(2.0)]))
            .unwrap();
        assert_eq!(get_int(&e, h, "hp"), Some(100));
        assert!(e.has(h, "pos"));
        assert!(!e.has(h, "нет"));
        assert!(e.get(h, "нет").is_none());
        assert!(e.del(h, "hp"));
        assert!(!e.del(h, "hp"));
        assert!(!e.has(h, "hp"));
        assert_eq!(e.count(), 1);
    }

    #[test]
    fn kill_reuses_slot_and_invalidates_old_handle() {
        let mut e = Ecs::new();
        let h1 = e.make();
        e.set(h1, "hp", V::Int(1)).unwrap();
        assert!(e.kill(h1));
        assert!(!e.kill(h1)); // повторный kill — уже мёртв
        assert!(!e.is_alive(h1));
        let h2 = e.make();
        assert_eq!(h1 & 0xFFFF_FFFF, h2 & 0xFFFF_FFFF, "слот переиспользован");
        assert_ne!(h1, h2, "поколение выросло");
        assert!(!e.is_alive(h1), "старый хендл не должен оживать");
        assert!(e.is_alive(h2));
        assert_eq!(get_int(&e, h2, "hp"), None, "компоненты вычищены");
        assert_eq!(e.count(), 1);
    }

    #[test]
    fn query_slot_order_and_all_components() {
        let mut e = Ecs::new();
        let mut hs = Vec::new();
        for k in 0..5u64 {
            let h = e.make();
            e.set(h, "pos", V::Int(k as i64)).unwrap();
            if k % 2 == 0 {
                e.set(h, "hp", V::Int(10)).unwrap();
            }
            hs.push(h);
        }
        let q1 = e.query(&["pos"]);
        assert_eq!(q1, hs, "запрос по слотам — детерминированный порядок");
        let q2 = e.query(&["pos", "hp"]);
        assert_eq!(q2, vec![hs[0], hs[2], hs[4]]);
        assert!(e.query(&["нет такого"]).is_empty());
        // после смерти одной сущности запрос сужается
        e.kill(hs[2]);
        assert_eq!(e.query(&["pos", "hp"]), vec![hs[0], hs[4]]);
    }

    #[test]
    fn dead_entity_set_get_errors() {
        let mut e = Ecs::new();
        let h = e.make();
        e.kill(h);
        assert!(e.set(h, "hp", V::Int(1)).is_err());
        assert_eq!(get_int(&e, h, "hp"), None);
        assert!(!e.del(h, "hp"));
        assert!(e.comps_of(h).is_empty());
    }

    #[test]
    fn comps_of_lists_names() {
        let mut e = Ecs::new();
        let h = e.make();
        e.set(h, "hp", V::Int(1)).unwrap();
        e.set(h, "tag", V::Str("coco".into())).unwrap();
        let mut names = e.comps_of(h);
        names.sort();
        assert_eq!(names, vec!["hp".to_string(), "tag".to_string()]);
    }
}
