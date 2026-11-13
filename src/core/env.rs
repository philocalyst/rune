use super::gc::{Context, GcState, ObjectMap, RootedDeref, Rt, Rto, Slot, Trace};
use super::object::{LispBuffer, Object, OpenBuffer, Symbol, WithLifetime};
use anyhow::{Result, anyhow};
use rune_macros::Trace;
use std::cell::OnceCell;

mod stack;
mod symbol_map;
pub(crate) use stack::*;
pub(crate) use symbol_map::*;

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
    pub(crate) current_buffer: CurrentBuffer<'a>,
    pub(crate) stack: LispStack<'a>,
}

#[derive(Debug)]
pub(crate) struct CurrentBuffer<'a> {
    buffer: OnceCell<OpenBuffer<'a>>,
    pub(crate) buf_ref: &'a LispBuffer,
}

// CurrentBuffer contains a globally stable buffer handle and an optional
// guard, not directly borrowed GC pointers. Its property slots are traced via
// OpenBuffer, so rooting does not require a transformed field representation.
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
    fn trace(&self, state: &mut GcState) {
        // LispBuffer itself lives in global storage and skips GC tracing.
        // Trace the current buffer's local property values explicitly, without
        // trying to lock a mutex whose guard this environment already holds.
        if let Some(buffer) = self.buffer.get() {
            buffer.trace(state);
        } else if let Ok(buffer) = self.buf_ref.lock() {
            buffer.trace(state);
        }
        // This is only a root for this environment's current buffer. It does
        // not establish ownership of non-current or cross-context buffers.
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
            env.current_buffer.get_mut().insert(cx.add("x")).unwrap();
            let key = cx.add(intern("m0-buffer-gc-property", cx));
            // The stored list/string have no root except the current buffer.
            let plist =
                Cons::new(key, Cons::new(cx.add("survives-current-buffer-gc"), NIL, cx), cx);
            env.current_buffer.get_mut().textprops_with_lifetime().set_properties(
                1,
                2,
                plist.into(),
            );
            if release {
                env.current_buffer.release();
            }
            for _ in 0..3 {
                cx.garbage_collect(true);
                let properties = *env.current_buffer.get().textprops.find(1).unwrap().val;
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
            // Remove context-local values before this global buffer outlives
            // its context; non-current-buffer ownership remains separate work.
            env.current_buffer.get_mut().textprops_with_lifetime().set_properties(1, 2, NIL);
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
