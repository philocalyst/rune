use super::gc::{Context, GcState, ObjectMap, RootedDeref, Rt, Rto, Slot, Trace};
use super::object::{LispBuffer, Object, OpenBuffer, Symbol, WithLifetime};
use anyhow::{Result, anyhow};
use rune_macros::Trace;
use std::cell::OnceCell;
use std::sync::{LazyLock, Mutex};

mod stack;
mod symbol_map;
pub(crate) use stack::*;
pub(crate) use symbol_map::*;

/// Interned feature symbols have global identity and stable storage, so provide
/// in a worker remains visible to other Envs. Heap-local uninterned features
/// instead live in each Env's traced feature slots below.
#[derive(Default)]
struct InternedFeatures(Vec<Symbol<'static>>);

// SAFETY: Only provide_feature inserts entries, after checking interned().
// All entries point into nonmoving global symbol storage; the mutex serializes
// access. No arbitrary Symbol or moving uninterned name can enter this table.
unsafe impl Send for InternedFeatures {}

static INTERNED_FEATURES: LazyLock<Mutex<InternedFeatures>> = LazyLock::new(Mutex::default);

type PropertyMap<'a> = ObjectMap<Slot<Symbol<'a>>, Vec<(Slot<Symbol<'a>>, Slot<Object<'a>>)>>;
#[derive(Debug, Default, Trace)]
pub(crate) struct Env<'a> {
    pub(crate) vars: ObjectMap<Slot<Symbol<'a>>, Slot<Object<'a>>>,
    pub(crate) props: PropertyMap<'a>,
    pub(crate) catch_stack: Vec<Slot<Object<'a>>>,
    exception: (Slot<Object<'a>>, Slot<Object<'a>>),
    #[no_trace]
    exception_id: u32,
    binding_stack: Vec<(Slot<Symbol<'a>>, Option<Slot<Object<'a>>>)>,
    pub(crate) match_data: Slot<Object<'a>>,
    features: Vec<Slot<Symbol<'a>>>,
    pub(crate) current_buffer: CurrentBuffer<'a>,
    pub(crate) stack: LispStack<'a>,
}

#[derive(Debug)]
pub(crate) struct CurrentBuffer<'a> {
    buffer: OnceCell<OpenBuffer<'a>>,
    pub(crate) buf_ref: &'a LispBuffer,
}

// CurrentBuffer contains a globally stable buffer handle and an optional
// guard, not directly borrowed GC pointers. Its property slots are owned by
// the Context registry, so rooting needs no transformed field representation.
impl RootedDeref for CurrentBuffer<'_> {
    type Target = Self;

    fn rooted_deref(rooted: &Rt<Self>) -> &Self {
        // Rt is repr(transparent); keep the lifetime parameter unchanged.
        unsafe { &*(rooted as *const Rt<Self>).cast::<Self>() }
    }

    fn rooted_derefmut(rooted: &mut Rt<Self>) -> &mut Self {
        unsafe { &mut *(rooted as *mut Rt<Self>).cast::<Self>() }
    }
}

impl PartialEq<LispBuffer> for Rt<CurrentBuffer<'_>> {
    fn eq(&self, other: &LispBuffer) -> bool {
        CurrentBuffer::eq(self, other)
    }
}

impl Trace for CurrentBuffer<'_> {
    fn trace(&self, _: &mut GcState) {
        // Globally stable text handle; Lisp metadata is traced by Context's
        // owner-domain registry even when this buffer is not current.
    }
}

impl Default for CurrentBuffer<'_> {
    fn default() -> Self {
        let name = crate::buffer::generate_new_buffer_name("*scratch*", None);
        let buffer = {
            // // need to drop global to avoid deadlocks
            let global = INTERNED_SYMBOLS.lock().unwrap();
            unsafe { global.create_buffer(&name).with_lifetime() }
        };
        crate::buffer::BUFFERS.lock().unwrap().insert(name, buffer);
        Self { buffer: Default::default(), buf_ref: buffer }
    }
}

impl<'a> CurrentBuffer<'a> {
    fn lock(&self) -> OpenBuffer<'a> {
        unsafe { self.buf_ref.lock().unwrap().with_lifetime() }
    }

    /// Fallible cached access for unwind/marker restoration. In particular,
    /// selecting a killed saved buffer must not panic or re-lock a held guard.
    pub(crate) fn try_get(&self) -> Result<&OpenBuffer<'a>> {
        if self.buffer.get().is_none() {
            let locked = unsafe { self.buf_ref.lock()?.with_lifetime() };
            let _ = self.buffer.set(locked);
        }
        let buffer = self.buffer.get().unwrap();
        anyhow::ensure!(buffer.is_live(), "selecting deleted buffer");
        Ok(buffer)
    }

    pub(crate) fn try_get_mut(&mut self) -> Result<&mut OpenBuffer<'a>> {
        self.try_get()?;
        Ok(self.buffer.get_mut().unwrap())
    }

    pub(crate) fn get(&self) -> &OpenBuffer<'a> {
        self.buffer.get_or_init(|| self.lock())
    }

    pub(crate) fn get_mut(&mut self) -> &mut OpenBuffer<'a> {
        // there is no get_or_init_mut
        if self.buffer.get().is_none() {
            let locked = self.lock();
            let _ = self.buffer.set(locked);
        }
        self.buffer.get_mut().unwrap()
    }

    pub(crate) fn set(&mut self, buffer: &LispBuffer) {
        let buffer = unsafe { buffer.with_lifetime() };
        self.buf_ref = buffer;
        self.release();
    }

    pub(crate) fn release(&mut self) {
        self.buffer.take();
    }
}

impl PartialEq<LispBuffer> for CurrentBuffer<'_> {
    fn eq(&self, other: &LispBuffer) -> bool {
        self.buf_ref == other
    }
}

// RootedEnv created by #[derive(Trace)]
impl<'a> RootedEnv<'a> {
    pub(crate) fn has_feature(&self, feature: Symbol, cx: &Context) -> bool {
        if feature.get().interned() {
            return INTERNED_FEATURES.lock().unwrap().0.contains(&feature);
        }
        self.features.iter().any(|provided| provided.bind(cx) == feature)
    }

    pub(crate) fn provide_feature(&mut self, feature: Symbol, cx: &Context) {
        if feature.get().interned() {
            let mut features = INTERNED_FEATURES.lock().unwrap();
            if !features.0.contains(&feature) {
                // The interned() check proves globally allocated stable storage.
                features.0.push(unsafe { feature.with_lifetime() });
            }
        } else if !self.has_feature(feature, cx) {
            self.features.push(feature);
        }
    }

    pub(crate) fn set_var(&mut self, sym: Symbol, value: Object) -> Result<()> {
        if sym.is_const() {
            Err(anyhow!("Attempt to set a constant symbol: {sym}"))
        } else {
            self.vars.insert(sym, value);
            Ok(())
        }
    }

    pub(crate) fn set_prop(&mut self, symbol: Symbol, propname: Symbol, value: Object) {
        match self.props.get_mut(symbol) {
            Some(plist) => match plist.iter_mut().find(|x| x.0 == propname) {
                Some(x) => x.1.set(value),
                None => plist.push((propname, value)),
            },
            None => {
                self.props.insert(symbol, vec![(propname, value)]);
            }
        }
    }

    pub(crate) fn set_exception(&mut self, tag: Object, data: Object) -> u32 {
        self.exception.0.set(tag);
        self.exception.1.set(data);
        self.exception_id += 1;
        self.exception_id
    }

    pub(crate) fn get_exception(&self, id: u32) -> Option<(&Rto<Object<'a>>, &Rto<Object<'a>>)> {
        (id == self.exception_id).then_some((&self.exception.0, &self.exception.1))
    }

    pub(crate) fn varbind(&mut self, var: Symbol, value: Object, cx: &Context) {
        let prev_value = self.vars.get(var).map(|x| x.bind(cx));
        self.binding_stack.push((var, prev_value));
        self.vars.insert(var, value);
    }

    pub(crate) fn unbind(&mut self, count: u16, cx: &Context) {
        for _ in 0..count {
            match self.binding_stack.bind_mut(cx).pop() {
                Some((sym, val)) => match val {
                    Some(val) => self.vars.insert(*sym, *val),
                    None => self.vars.remove(*sym),
                },
                None => panic!("Binding stack was empty"),
            }
        }
    }

    pub(crate) fn defvar(&mut self, var: Symbol, value: Object) -> Result<()> {
        // TOOD: Handle `eval-sexp` on defvar, which should always update the
        // value
        if self.vars.get(var).is_none() {
            self.set_var(var, value)?;
            var.make_special();
        }

        // If this variable was unbound previously in the binding stack,
        // we will bind it to the new value
        for binding in &mut *self.binding_stack {
            if binding.0 == var && binding.1.is_none() {
                binding.1.set(Some(value));
            }
        }
        Ok(())
    }

    pub(crate) fn set_buffer(&mut self, buffer: &LispBuffer) {
        if buffer == self.current_buffer.buf_ref {
            return;
        }
        CurrentBuffer::set(&mut self.current_buffer, buffer);
    }

    pub(crate) fn with_buffer<T>(
        &self,
        buffer: &LispBuffer,
        mut func: impl FnMut(&OpenBuffer) -> T,
    ) -> Result<T> {
        if self.current_buffer == *buffer {
            Ok(func(self.current_buffer.get()))
        } else {
            let buffer = buffer.lock()?;
            Ok(func(&buffer))
        }
    }

    pub(crate) fn with_buffer_mut<T>(
        &mut self,
        buffer: &LispBuffer,
        mut func: impl FnMut(&mut OpenBuffer) -> T,
    ) -> Result<T> {
        if self.current_buffer == *buffer {
            Ok(func(self.current_buffer.get_mut()))
        } else {
            let mut buffer = buffer.lock()?;
            Ok(func(&mut buffer))
        }
    }
}

#[cfg(test)]
mod current_buffer_gc_tests {
    use super::*;
    use crate::core::{cons::Cons, gc::RootSet, object::NIL};
    use rune_core::macros::root;

    #[test]
    fn m0_current_buffer_properties_survive_gc_with_cached_and_released_guard() {
        for release in [false, true] {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            root!(env, new(Env), cx);
            env.current_buffer.get_mut().insert(cx.add("x"), &cx).unwrap();
            let key = cx.add(intern("m0-buffer-gc-property", cx));
            // The stored list/string have no root except the current buffer.
            let plist =
                Cons::new(key, Cons::new(cx.add("survives-current-buffer-gc"), NIL, cx), cx);
            env.current_buffer.get_mut().textprops.lock(&cx).unwrap().set_properties(
                1,
                2,
                plist.into(),
            );
            if release {
                env.current_buffer.release();
            }
            for _ in 0..3 {
                cx.garbage_collect(true);
                let properties =
                    *env.current_buffer.get().textprops.lock(&cx).unwrap().find(1).unwrap().val;
                let value = crate::intervals::textget(
                    properties,
                    cx.add(intern("m0-buffer-gc-property", cx)),
                )
                .unwrap();
                assert_eq!(value, cx.add("survives-current-buffer-gc"));
                if release {
                    env.current_buffer.release();
                }
            }
            // Context teardown retires metadata even while its global handle survives.
        }
    }

    #[test]
    fn m0_current_buffer_gc_tolerates_killed_guard() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, new(Env), cx);
        env.current_buffer.get_mut().kill();
        cx.garbage_collect(true);
        env.current_buffer.release();
        cx.garbage_collect(true);
    }
}

#[cfg(test)]
mod feature_ownership_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;

    #[test]
    fn interned_features_provided_by_worker_remain_globally_visible() {
        let name = "ownership-global-worker-feature";
        std::thread::spawn(move || {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            root!(env, new(Env), cx);
            let feature = intern(name, cx);
            env.provide_feature(feature, cx);
            assert!(env.features.is_empty());
        })
        .join()
        .unwrap();
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, new(Env), cx);
        assert!(env.has_feature(intern(name, cx), cx));
    }

    #[test]
    fn uninterned_features_are_rooted_and_keep_identity() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, new(Env), cx);
        {
            let first = Symbol::new_uninterned("local-feature", &cx.block);
            let second = Symbol::new_uninterned("local-feature", &cx.block);
            env.provide_feature(first, cx);
            env.provide_feature(first, cx);
            env.provide_feature(second, cx);
        }
        assert_eq!(env.features.len(), 2);
        for _ in 0..10 {
            cx.garbage_collect(true);
            let first = env.features[0].bind(cx);
            let second = env.features[1].bind(cx);
            assert_ne!(first, second);
            assert_eq!(first.get().name(), "local-feature");
            assert_eq!(second.get().name(), "local-feature");
            assert!(env.has_feature(first, cx));
            assert!(env.has_feature(second, cx));
            let fresh = Symbol::new_uninterned("local-feature", &cx.block);
            assert!(!env.has_feature(fresh, cx));
        }
    }
}
