use super::gc::{Context, ObjectMap, Rt, Rto, Slot, Trace};
use super::object::{LispBuffer, Number, Object, Symbol, WithLifetime};
use crate::editor_document::{
    BufferRead, MarkerCleanupGuard, OwnerBufferHandle, RuntimeEditorGateway,
};
use anyhow::{Result, anyhow};
use rune_macros::Trace;
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
#[derive(Debug, Trace)]
pub(crate) struct Env<'a> {
    pub(crate) vars: ObjectMap<Slot<Symbol<'a>>, Slot<Object<'a>>>,
    pub(crate) props: PropertyMap<'a>,
    pub(crate) catch_stack: Vec<Slot<Object<'a>>>,
    binding_stack: Vec<(Slot<Symbol<'a>>, Option<Slot<Object<'a>>>)>,
    pub(crate) match_data: Slot<Object<'a>>,
    features: Vec<Slot<Symbol<'a>>>,
    #[no_trace]
    pub(crate) current_buffer: OwnerBufferHandle,
    #[no_trace]
    editor_gateway: RuntimeEditorGateway,
    pub(crate) stack: LispStack<'a>,
}

/// Opaque live registration plus a native-only panic cleanup intent. It never
/// borrows Context, so evaluation/collection can run while the token exists.
pub(crate) struct ExcursionToken {
    buffer: OwnerBufferHandle,
    marker: rune_editor::MarkerId,
    cleanup: MarkerCleanupGuard,
}
impl<'a> Env<'a> {
    pub(crate) fn new(cx: &Context) -> Result<Self> {
        let editor_gateway = cx.editor_gateway()?;
        let current_buffer = editor_gateway.scratch(cx)?;
        Ok(Self {
            vars: Default::default(),
            props: Default::default(),
            catch_stack: Default::default(),
            binding_stack: Default::default(),
            match_data: Default::default(),
            features: Default::default(),
            current_buffer,
            editor_gateway,
            stack: Default::default(),
        })
    }
    /// Tests asking for isolation choose a fresh buffer explicitly while still
    /// sharing the Context's single owner. Production new binds owner scratch.
    #[cfg(test)]
    pub(crate) fn new_isolated(cx: &Context) -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| n.checked_add(1),
            )
            .expect("test buffer IDs exhausted");
        let mut env = Self::new(cx)?;
        env.current_buffer =
            env.editor_gateway.create(&format!(" *isolated-eval-{id}*"), "", cx)?;
        Ok(env)
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

    pub(crate) fn validate_variable_value(var: Symbol, value: Object) -> Result<()> {
        if var == crate::sym::INTEGER_WIDTH {
            Number::try_from(value)?.integer_width()?;
        }
        Ok(())
    }

    pub(crate) fn set_var(&mut self, sym: Symbol, value: Object) -> Result<()> {
        Self::validate_variable_value(sym, value)?;
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

    pub(crate) fn varbind(&mut self, var: Symbol, value: Object, cx: &Context) -> Result<()> {
        Self::validate_variable_value(var, value)?;
        let prev_value = self.vars.get(var).map(|x| x.bind(cx));
        self.binding_stack.push((var, prev_value));
        self.vars.insert(var, value);
        Ok(())
    }

    /// Save a dynamic-binding boundary for bytecode frames and handlers.
    pub(crate) fn binding_depth(&self) -> usize {
        self.binding_stack.len()
    }

    /// Restore a previously saved boundary without partially mutating on error.
    pub(crate) fn unwind_bindings(&mut self, depth: usize, cx: &Context) -> Result<()> {
        anyhow::ensure!(depth <= self.binding_depth(), "Invalid dynamic binding checkpoint");
        while self.binding_depth() > depth {
            self.unbind(1, cx);
        }
        Ok(())
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
                Self::validate_variable_value(var, value)?;
                binding.1.set(Some(value));
            }
        }
        Ok(())
    }

    pub(crate) fn editor_gateway(&self) -> RuntimeEditorGateway {
        self.editor_gateway.clone()
    }
    pub(crate) fn current_buffer_handle(&self) -> OwnerBufferHandle {
        self.current_buffer
    }
    pub(crate) fn buffer_read(&self, cx: &Context) -> Result<BufferRead> {
        self.editor_gateway.read(self.current_buffer, cx)
    }
    pub(crate) fn set_current_buffer(
        &mut self,
        handle: OwnerBufferHandle,
        cx: &Context,
    ) -> Result<()> {
        self.editor_gateway.read(handle, cx)?;
        self.current_buffer = handle;
        Ok(())
    }
    pub(crate) fn set_buffer(&mut self, buffer: &LispBuffer, cx: &Context) -> Result<()> {
        self.set_current_buffer(buffer.handle(), cx)
    }
    pub(crate) fn save_excursion(&self, cx: &Context) -> Result<ExcursionToken> {
        let read = self.buffer_read(cx)?;
        let (marker, cleanup) = self.editor_gateway.register_temporary_marker(
            self.current_buffer,
            read.point,
            rune_editor::Bias::Left,
            cx,
        )?;
        Ok(ExcursionToken { buffer: self.current_buffer, marker, cleanup })
    }
    /// Restore CURRENT live marker state, not an old immutable marker root.
    /// The guard's Drop queues unregister if any failure interrupts cleanup.
    pub(crate) fn restore_excursion(&mut self, token: ExcursionToken, cx: &Context) -> bool {
        let Ok(Some(point)) = self.editor_gateway.marker_position(token.buffer, token.marker, cx)
        else {
            return false;
        };
        if self.editor_gateway.remove_marker(token.buffer, token.marker, cx).is_err() {
            return false;
        }
        if self.editor_gateway.set_point(token.buffer, point, cx).is_err() {
            return false;
        }
        self.current_buffer = token.buffer;
        token.cleanup.disarm();
        true
    }
    pub(crate) fn with_buffer<T>(
        &self,
        buffer: &LispBuffer,
        cx: &Context,
        func: impl FnOnce(&BufferRead) -> T,
    ) -> Result<T> {
        let read = self.editor_gateway.read(buffer.handle(), cx)?;
        Ok(func(&read))
    }
}

#[cfg(test)]
mod owner_env_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn independent_env_bindings_share_exact_owner_without_accidental_buffer_aliases() -> Result<()>
    {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(first, init(Env::new(cx)?), cx);
        root!(second, init(Env::new(cx)?), cx);
        assert_eq!(first.current_buffer_handle(), second.current_buffer_handle());
        let gateway = first.editor_gateway();
        let a = gateway.create("env-a", "λ", cx)?;
        let b = gateway.create("env-b", "🙂", cx)?;
        first.set_current_buffer(a, cx)?;
        second.set_current_buffer(b, cx)?;
        let read = first.buffer_read(cx)?;
        gateway.splice(a, read.epoch, 1..1, "x", cx)?;
        assert_eq!(first.buffer_read(cx)?.document.text().to_string(), "λx");
        assert_eq!(second.buffer_read(cx)?.document.text().to_string(), "🙂");
        assert_eq!(second.current_buffer_handle(), b);
        root!(isolated, init(Env::new_isolated(cx)?), cx);
        assert_ne!(isolated.current_buffer_handle(), a);
        assert_ne!(isolated.current_buffer_handle(), b);
        cx.garbage_collect(true);
        assert_eq!(first.buffer_read(cx)?.document.text().to_string(), "λx");
        Ok(())
    }
    #[test]
    fn excursion_cleanup_guard_queues_only_abandoned_native_registration() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new_isolated(cx)?), cx);
        let gateway = env.editor_gateway();
        let handle = env.current_buffer_handle();
        let keep = gateway.register_marker(handle, 0, rune_editor::Bias::Left, cx)?;
        let mut abandoned = None;
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let token = env.save_excursion(cx).unwrap();
            abandoned = Some(token.marker);
            let _guard = token;
            panic!("test unwind");
        }));
        assert!(panic.is_err());
        assert_eq!(gateway.marker_position(handle, abandoned.unwrap(), cx)?, None);
        assert_eq!(gateway.marker_position(handle, keep, cx)?, Some(0));
        Ok(())
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
            root!(env, init(Env::new(cx).unwrap()), cx);
            let feature = intern(name, cx);
            env.provide_feature(feature, cx);
            assert!(env.features.is_empty());
        })
        .join()
        .unwrap();
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        assert!(env.has_feature(intern(name, cx), cx));
    }

    #[test]
    fn uninterned_features_are_rooted_and_keep_identity() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
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
