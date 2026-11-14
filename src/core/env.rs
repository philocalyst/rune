use super::gc::{Context, ObjectMap, Rt, Rto, Slot, Trace};
use super::object::{LispBuffer, Number, Object, Symbol, WithLifetime};
use crate::editor_document::{
    BufferRead, MarkerCleanupGuard, OwnerBufferHandle, RuntimeEditorGateway,
};
use anyhow::{Result, anyhow};
use rune_macros::Trace;
use std::sync::{LazyLock, Mutex};

mod quit;
pub(crate) use quit::QuitTokenGuard;
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
    // Same-depth native target metadata. None denotes an ordinary Env binding.
    #[no_trace]
    binding_targets: Vec<Option<OwnerBufferHandle>>,
    pub(crate) match_data: Slot<Object<'a>>,
    pub(crate) native_key_prefix: Slot<Object<'a>>,
    pub(crate) keymap_functions: ObjectMap<Slot<Symbol<'a>>,Slot<Object<'a>>>,
    #[no_trace]
    pub(crate) native_key_sequence: Option<u64>,
    features: Vec<Slot<Symbol<'a>>>,
    #[no_trace]
    pub(crate) current_buffer: OwnerBufferHandle,
    #[no_trace]
    editor_gateway: RuntimeEditorGateway,
    #[no_trace]
    quit: quit::QuitControl,
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
            binding_targets: Default::default(),
            match_data: Default::default(),
            native_key_prefix: Default::default(),
            keymap_functions: Default::default(),
            native_key_sequence: None,
            features: Default::default(),
            current_buffer,
            editor_gateway,
            quit: Default::default(),
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
    pub(crate) fn initialize_condition_metadata(&mut self, cx: &Context) {
        use super::cons::Cons;
        use super::object::NIL;
        let simple = [
            sym::WRONG_NUMBER_OF_ARGUMENTS,
            sym::WRONG_TYPE_ARGUMENT,
            sym::VOID_VARIABLE,
            sym::VOID_FUNCTION,
            sym::INVALID_FUNCTION,
            sym::SETTING_CONSTANT,
            sym::BUFFER_READ_ONLY,
            sym::ARITH_ERROR,
            sym::RUNE_UNIMPLEMENTED,
        ];
        let mut entries = vec![(sym::ERROR, vec![sym::ERROR]), (sym::QUIT, vec![sym::QUIT])];
        entries.extend(simple.into_iter().map(|symbol| (symbol, vec![symbol, sym::ERROR])));
        entries.push((sym::RANGE_ERROR, vec![sym::RANGE_ERROR, sym::ARITH_ERROR, sym::ERROR]));
        entries.push((
            sym::OVERFLOW_ERROR,
            vec![sym::OVERFLOW_ERROR, sym::RANGE_ERROR, sym::ARITH_ERROR, sym::ERROR],
        ));
        for (symbol, classes) in entries {
            if self
                .props
                .get(symbol)
                .is_some_and(|properties| properties.iter().any(|p| p.0 == sym::ERROR_CONDITIONS))
            {
                continue;
            }
            let mut list = NIL;
            for class in classes.into_iter().rev() {
                list = Cons::new(class, list, cx).into();
            }
            self.set_prop(symbol, sym::ERROR_CONDITIONS, list);
        }
    }

    pub(crate) fn install_quit_token(
        &self,
        token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> QuitTokenGuard {
        self.quit.install(token)
    }
    pub(crate) fn poll_quit(&self, cx: &Context) -> Result<(), crate::eval::EvalError> {
        let inhibited = self.vars.get(sym::INHIBIT_QUIT).is_some_and(|v| !v.bind(cx).is_nil());
        if self.quit.consume(inhibited) {
            return Err(crate::eval::EvalError::signal(
                sym::QUIT.into(),
                crate::core::object::NIL,
                cx,
            ));
        }
        Ok(())
    }
    pub(crate) fn inhibit_cleanup_quit(&self) -> quit::CleanupGuard {
        self.quit.cleanup()
    }

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
        if var.is_builtin_value_constant() {
            return Err(anyhow!("Attempt to set a constant symbol: {var}"));
        }
        if var == crate::sym::INTEGER_WIDTH {
            Number::try_from(value)?.integer_width()?;
        }
        Ok(())
    }

    /// Read forwarded variables from the Context's single buffer authority.
    pub(crate) fn variable_value<'cx>(
        &self,
        symbol: Symbol,
        cx: &'cx Context,
    ) -> Result<Option<Object<'cx>>> {
        if symbol == crate::sym::BUFFER_READ_ONLY {
            return self.editor_gateway.read_only_value(self.current_buffer, cx).map(Some);
        }
        Ok(self.vars.get(symbol).map(|value| value.bind(cx)))
    }

    /// Context-aware variable assignment. No buffer value is mirrored in vars.
    pub(crate) fn set_var_context(
        &mut self,
        symbol: Symbol,
        value: Object,
        cx: &Context,
    ) -> Result<()> {
        Self::validate_variable_value(symbol, value)?;
        if symbol == crate::sym::BUFFER_READ_ONLY {
            // SAFETY: Env assignment callers supply values rooted in this owning
            // Context (evaluation slots or Context-bound native primitive args).
            return unsafe {
                self.editor_gateway.set_read_only_value(self.current_buffer, value, cx)
            };
        }
        self.set_var(symbol, value)
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
        assert_eq!(
            self.binding_stack.len(),
            self.binding_targets.len(),
            "Dynamic binding target stack mismatch"
        );
        self.binding_stack.bind_mut(cx).try_reserve(1)?;
        self.binding_targets.try_reserve(1)?;
        let target = (var == crate::sym::BUFFER_READ_ONLY).then_some(self.current_buffer);
        let prev_value = self.variable_value(var, cx)?;
        // Prepare and publish the forwarded write before pushing its unwind record.
        // No Lisp allocation or callback occurs between publication and both pushes.
        self.set_var_context(var, value, cx)?;
        self.binding_stack.push((var, prev_value));
        self.binding_targets.push(target);
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
            self.try_unbind(1, cx)?;
        }
        Ok(())
    }

    /// Restore bindings without discarding an unwind record if the owner
    /// rejects its restoration (busy, foreign Context, sequence exhaustion).
    pub(crate) fn try_unbind(&mut self, count: u16, cx: &Context) -> Result<()> {
        anyhow::ensure!(usize::from(count) <= self.binding_stack.len(), "Binding stack was empty");
        for _ in 0..count {
            assert_eq!(
                self.binding_stack.len(),
                self.binding_targets.len(),
                "Dynamic binding target stack mismatch"
            );
            let target = *self.binding_targets.last().expect("Binding stack was empty");
            let (symbol, value) = self
                .binding_stack
                .bind_ref(cx)
                .last()
                .map(|(symbol, value)| (**symbol, value.as_ref().map(|value| **value)))
                .expect("Binding stack was empty");
            if let Some(target) = target {
                if let Some(value) = value {
                    // SAFETY: value remains in its traced binding Slot until
                    // restoration succeeds. No GC/callback occurs in the gateway.
                    unsafe { self.editor_gateway.restore_read_only_value(target, value, cx) }?;
                }
            } else {
                match value {
                    Some(value) => self.vars.insert(symbol, value),
                    None => self.vars.remove(symbol),
                }
            }
            self.binding_stack.pop();
            self.binding_targets.pop();
        }
        Ok(())
    }

    pub(crate) fn unbind(&mut self, count: u16, cx: &Context) {
        self.try_unbind(count, cx).expect("dynamic binding restoration failed");
    }

    pub(crate) fn toplevel_value<'ob>(&self, var: Symbol, cx: &'ob Context) -> Option<Object<'ob>> {
        if var.is_const() {
            return Some(var.into());
        }
        for ((symbol, saved), target) in
            self.binding_stack.bind_ref(cx).iter().zip(&self.binding_targets)
        {
            if target.is_none() && **symbol == var {
                return saved.as_ref().map(|value| cx.bind(**value));
            }
        }
        self.vars.get(var).map(|value| value.bind(cx))
    }
    pub(crate) fn set_toplevel_value(&mut self, var: Symbol, value: Object) -> Result<()> {
        Self::validate_variable_value(var, value)?;
        if var.is_const() {
            return Err(anyhow!("Attempt to set a constant symbol"));
        }
        for (binding, target) in self.binding_stack.iter_mut().zip(&self.binding_targets) {
            if target.is_none() && binding.0 == var {
                binding.1.set(Some(value));
                return Ok(());
            }
        }
        self.set_var(var, value)
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
        for (binding, target) in self.binding_stack.iter_mut().zip(&self.binding_targets) {
            if target.is_none() && binding.0 == var && binding.1.is_none() {
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

#[cfg(test)]
mod builtin_value_constant_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::{rebind, root};

    #[test]
    fn numeric_bounds_are_readonly_values_not_readonly_function_cells() -> Result<()> {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx)?), cx);
        // Startup is privileged: generated initializers insert the builtin
        // values directly rather than using user assignment/binding APIs.
        init_variables(cx, env);
        for var in [sym::MOST_POSITIVE_FIXNUM, sym::MOST_NEGATIVE_FIXNUM] {
            let before = env.vars.get(var).unwrap().bind(cx);
            root!(before, cx);
            let depth = env.binding_depth();
            assert!(var.is_builtin_value_constant());
            assert!(!var.is_const(), "value policy must not constrain function cells");
            assert!(env.set_var(var, cx.add(1_i64)).is_err());
            assert!(env.varbind(var, cx.add(1_i64), cx).is_err());
            assert_eq!(env.binding_depth(), depth);
            assert_eq!(env.vars.get(var).unwrap().bind(cx), before.bind(cx));
            // Function namespace stays writable; restore any original slot
            // before asserting so the global test table is not left changed.
            let original = var.func(cx).map(|f| f.as_obj_copy()).unwrap_or(crate::sym::NIL.into());
            let assigned = crate::data::fset(var, cx.add(sym::IDENTITY));
            let restored = crate::data::fset(var, original);
            assert!(assigned.is_ok());
            assert!(restored.is_ok());
        }
        let same_name = Symbol::new_uninterned("most-positive-fixnum", &cx.block);
        assert!(!same_name.is_builtin_value_constant());
        env.set_var(same_name, cx.add(1_i64))?;
        for source in [
            "(condition-case nil (setq most-positive-fixnum 1) (error 'caught))",
            "(condition-case nil (set 'most-negative-fixnum 0) (error 'caught))",
            "(condition-case nil (let ((most-positive-fixnum 1)) 1) (error 'caught))",
            "(condition-case nil (setq most-positive-fixnum most-positive-fixnum) (error 'caught))",
        ] {
            let form = crate::reader::read(source, cx)?.0;
            root!(form, cx);
            let result = rebind!(crate::interpreter::eval(form, None, env, cx)?, cx);
            assert_eq!(result, intern("caught", cx));
        }
        // Lexical lambda/handler slots may shadow these names, as in GNU's
        // lexical mode. The gate applies to actual global/dynamic writes.
        for source in [
            "(funcall (function (lambda (most-positive-fixnum) (setq most-positive-fixnum 2))) 1)",
            "(condition-case most-positive-fixnum (if) (error (setq most-positive-fixnum 2)))",
        ] {
            let form = crate::reader::read(source, cx)?.0;
            root!(form, cx);
            let result = rebind!(crate::interpreter::eval(form, None, env, cx)?, cx);
            assert_eq!(result, Object::from(2_i64));
        }
        Ok(())
    }
}

#[cfg(test)]
mod condition_bootstrap_tests {
    use super::*;
    use crate::core::gc::RootSet;
    use rune_core::macros::root;
    #[test]
    fn builtin_condition_metadata_is_rooted_per_env_and_keeps_explicit_overrides() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(first, init(Env::new(cx).unwrap()), cx);
        root!(second, init(Env::new(cx).unwrap()), cx);
        init_variables(cx, first);
        init_variables(cx, second);
        for (symbol, expected) in [
            (sym::ERROR, "(error)"),
            (sym::QUIT, "(quit)"),
            (sym::WRONG_TYPE_ARGUMENT, "(wrong-type-argument error)"),
            (sym::WRONG_NUMBER_OF_ARGUMENTS, "(wrong-number-of-arguments error)"),
            (sym::VOID_VARIABLE, "(void-variable error)"),
            (sym::VOID_FUNCTION, "(void-function error)"),
            (sym::INVALID_FUNCTION, "(invalid-function error)"),
            (sym::SETTING_CONSTANT, "(setting-constant error)"),
            (sym::ARITH_ERROR, "(arith-error error)"),
            (sym::RANGE_ERROR, "(range-error arith-error error)"),
            (sym::OVERFLOW_ERROR, "(overflow-error range-error arith-error error)"),
        ] {
            let expected = crate::reader::read(expected, cx).unwrap().0;
            assert_eq!(crate::data::get(symbol, sym::ERROR_CONDITIONS, first, cx), expected);
        }
        first.set_prop(sym::WRONG_TYPE_ARGUMENT, sym::ERROR_CONDITIONS, crate::core::object::NIL);
        first.initialize_condition_metadata(cx);
        for _ in 0..5 {
            cx.garbage_collect(true);
        }
        assert!(
            crate::data::get(sym::WRONG_TYPE_ARGUMENT, sym::ERROR_CONDITIONS, first, cx).is_nil()
        );
        assert!(
            !crate::data::get(sym::WRONG_TYPE_ARGUMENT, sym::ERROR_CONDITIONS, second, cx).is_nil()
        );
        let signal = crate::eval::EvalError::signal(
            sym::WRONG_TYPE_ARGUMENT.into(),
            crate::core::object::NIL,
            cx,
        );
        assert!(!signal.matches_handler(sym::ERROR.into(), first, cx).unwrap());
        assert!(signal.matches_handler(sym::ERROR.into(), second, cx).unwrap());
    }
}

#[cfg(test)]
mod forwarded_readonly_tests {
    use super::*;
    use crate::core::object::{NIL, TRUE};
    use crate::sym;
    use rune_core::macros::root;

    #[test]
    fn actual_lisp_readonly_blocks_insert_and_dynamic_override_restores_raw_value() -> Result<()> {
        use rune_core::macros::rebind;
        let roots = super::super::gc::RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        init_variables(cx, env);
        let source = "(progn (insert \"initial\") (setq buffer-read-only 'raw) (list (condition-case nil (progn (insert \"FORBIDDEN\") nil) (buffer-read-only t)) (let ((buffer-read-only nil)) (insert \" allowed\") buffer-read-only) buffer-read-only (buffer-substring-no-properties (point-min) (point-max))))";
        let form = crate::reader::read(source, cx)?.0;
        root!(form, cx);
        let result = rebind!(crate::interpreter::eval(form, None, env, cx)?, cx);
        assert_eq!(result.to_string(), "(t nil raw \"initial allowed\")");
        Ok(())
    }

    #[test]
    fn failed_foreign_gateway_unwind_retains_traced_binding_for_retry() -> Result<()> {
        let roots = super::super::gc::RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        env.set_var_context(sym::BUFFER_READ_ONLY, TRUE, cx)?;
        env.varbind(sym::BUFFER_READ_ONLY, NIL, cx)?;
        let original = env.current_buffer_handle();
        let other = RuntimeEditorGateway::new(cx, 80, 24)?;
        let foreign = other.create("foreign-target", "", cx)?;
        *env.binding_targets.last_mut().unwrap() = Some(foreign);
        assert!(env.try_unbind(1, cx).is_err());
        assert_eq!(env.binding_depth(), 1);
        assert_eq!(env.variable_value(sym::BUFFER_READ_ONLY, cx)?, Some(NIL));
        *env.binding_targets.last_mut().unwrap() = Some(original);
        env.try_unbind(1, cx)?;
        assert_eq!(env.binding_depth(), 0);
        assert_eq!(env.variable_value(sym::BUFFER_READ_ONLY, cx)?, Some(TRUE));
        Ok(())
    }

    #[test]
    fn forwarded_heap_value_survives_gc_and_killed_binding_never_targets_current() -> Result<()> {
        let roots = super::super::gc::RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx)?), cx);
        env.set_var_context(sym::BUFFER_READ_ONLY, cx.add("retained-raw"), cx)?;
        cx.garbage_collect(true);
        let raw = env.variable_value(sym::BUFFER_READ_ONLY, cx)?.unwrap();
        assert_eq!(<&str>::try_from(raw)?, "retained-raw");
        let original = env.current_buffer_handle();
        env.varbind(sym::BUFFER_READ_ONLY, NIL, cx)?;
        let other = env.editor_gateway().create(" *readonly-kill-other*", "", cx)?;
        env.set_current_buffer(other, cx)?;
        env.editor_gateway().kill(original, true, cx)?;
        env.unbind(1, cx);
        assert_eq!(env.variable_value(sym::BUFFER_READ_ONLY, cx)?, Some(NIL));
        Ok(())
    }

    #[test]
    fn readonly_is_shared_per_buffer_and_dynamic_restore_targets_original_buffer() -> Result<()> {
        let roots = super::super::gc::RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(first, init(Env::new_isolated(cx)?), cx);
        root!(second, init(Env::new(cx)?), cx);
        let original = first.current_buffer_handle();
        second.set_current_buffer(original, &cx)?;
        first.set_var_context(sym::BUFFER_READ_ONLY, TRUE, &cx)?;
        assert_eq!(second.variable_value(sym::BUFFER_READ_ONLY, &cx)?, Some(TRUE));
        first.varbind(sym::BUFFER_READ_ONLY, NIL, &cx)?;
        assert_eq!(second.variable_value(sym::BUFFER_READ_ONLY, &cx)?, Some(NIL));
        let other = first.editor_gateway().create(" *readonly-other*", "", &cx)?;
        first.set_current_buffer(other, &cx)?;
        first.unbind(1, &cx);
        assert_eq!(first.variable_value(sym::BUFFER_READ_ONLY, &cx)?, Some(NIL));
        assert_eq!(second.variable_value(sym::BUFFER_READ_ONLY, &cx)?, Some(TRUE));
        Ok(())
    }
}
