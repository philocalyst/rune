//! Lisp evaluation primitives.
use crate::core::cons::{Cons, ConsError};
use crate::core::env::{ArgSlice, CallFrame, Env, sym};
use crate::core::error::{Type, TypeError};
use crate::core::gc::{Rt, Rto};
use crate::core::object::{
    FnArgs, Function, HeapDomainValue, LispString, NIL, ObjectType, Symbol, TagType, WithLifetime,
};
use crate::core::{
    gc::Context,
    object::{FunctionType, Gc, Object},
};
use crate::data::LispError;
use crate::fns::{assq, eq};
use crate::rooted_iter;
use anyhow::{Result, anyhow, bail, ensure};
use fallible_iterator::FallibleIterator;
use fallible_streaming_iterator::FallibleStreamingIterator;
use rune_core::macros::{bail_err, call, list, root};
use rune_macros::defun;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub(crate) struct EvalError {
    backtrace: Vec<Box<str>>,
    pub(crate) error: ErrorType,
}

#[derive(Debug)]
pub(crate) enum ErrorType {
    Throw(PendingExit),
    Signal(PendingExit),
    Err(anyhow::Error),
}

/// Independently retained payload for one pending nonlocal exit. A cleanup
/// can raise and handle another exit without replacing this payload's GC root.
#[derive(Debug)]
pub(crate) struct PendingExit {
    payload: HeapDomainValue,
}

impl PendingExit {
    fn new(tag: Object, data: Object, cx: &Context) -> Self {
        let pair = Cons::new(tag, data, cx);
        Self {
            // SAFETY: pair is freshly allocated in cx; its inputs are the
            // current evaluator's owner-domain values (or audited globals).
            payload: unsafe { HeapDomainValue::new(pair.into(), cx) },
        }
    }

    /// Binding never reconstructs tag identity from a diagnostic. In another
    /// or retired owner domain a throw must fail closed, not match a new catch.
    pub(crate) fn bind<'ob>(&self, cx: &'ob Context) -> Result<&'ob Cons> {
        self.payload
            .bind(cx)?
            .cons()
            .ok_or_else(|| anyhow!("Invalid pending exit payload"))
    }

    pub(crate) fn condition<'ob>(&self, cx: &'ob Context) -> &'ob Cons {
        self.bind(cx).unwrap_or_else(|_| {
            Cons::new(sym::ERROR, "Signal payload unavailable in this heap domain", cx)
        })
    }
}

impl std::error::Error for EvalError {}

impl Display for EvalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.error {
            ErrorType::Err(e) => writeln!(f, "{e}")?,
            ErrorType::Throw(_) => writeln!(f, "No catch for throw")?,
            ErrorType::Signal(_) => writeln!(f, "Signal")?,
        }
        Ok(())
    }
}

// Error-path diagnostics must not invoke general Lisp printers: ByteFn
// constants may point back to the function, container locks may already be
// held, and even an acyclic argument graph may be arbitrarily deep/large.
const DIAGNOSTIC_ARGUMENTS: usize = 16;
const DIAGNOSTIC_CHARACTERS: usize = 64;

fn diagnostic_text(text: &str) -> String {
    let mut characters = text.chars();
    let mut output: String = characters
        .by_ref()
        .take(DIAGNOSTIC_CHARACTERS)
        .flat_map(char::escape_debug)
        .collect();
    if characters.next().is_some() {
        output.push('…');
    }
    output
}

fn diagnostic_object(object: Object) -> String {
    match object.untag() {
        ObjectType::Int(value) => value.to_string(),
        ObjectType::Symbol(value) => diagnostic_text(value.name()),
        ObjectType::String(value) => format!("\"{}\"", diagnostic_text(value)),
        // Type labels inspect only the tag: never follow graph edges, acquire
        // a Lisp container mutex, or format arbitrarily large integer payloads.
        value => format!("<{:?}>", value.get_type()),
    }
}

fn diagnostic_arguments(arguments: &[Rto<Object>]) -> String {
    let mut output = String::from("[");
    for (index, argument) in arguments.iter().take(DIAGNOSTIC_ARGUMENTS).enumerate() {
        if index != 0 {
            output.push(' ');
        }
        // SAFETY: this synchronous diagnostic path never collects, invokes
        // Lisp/extension callbacks, or retains the copied value. Rooted call
        // arguments remain in their owning thread; only bounded primitive
        // reads or tag inspection occur before the borrow ends.
        let object = unsafe { argument.bind_unchecked() };
        output.push_str(&diagnostic_object(object));
    }
    if arguments.len() > DIAGNOSTIC_ARGUMENTS {
        output.push_str(" …");
    }
    output.push(']');
    output
}

fn diagnostic_trace(name: &str, arguments: &[Rto<Object>]) -> Box<str> {
    format!("{} {}", diagnostic_text(name), diagnostic_arguments(arguments)).into_boxed_str()
}

fn condition_any(
    mut tail: Object<'_>,
    mut predicate: impl FnMut(Object<'_>) -> Result<bool>,
) -> Result<bool> {
    let mut seen = std::collections::HashSet::new();
    loop {
        match tail.untag() {
            ObjectType::NIL => return Ok(false),
            ObjectType::Cons(pair) => {
                ensure!(seen.insert(pair as *const Cons as usize), "Cyclic condition list");
                if predicate(pair.car())? {
                    return Ok(true);
                }
                tail = pair.cdr();
            }
            _ => bail!("Condition metadata must be a proper list"),
        }
    }
}

impl EvalError {
    pub(crate) fn new_error(error: anyhow::Error) -> Self {
        Self { backtrace: Vec::new(), error: ErrorType::Err(error) }
    }

    /// Callback-free condition matching. Property lists are scanned with cycle
    /// detection; no Lisp graph is rendered while selecting a handler.
    pub(crate) fn matches_handler(
        &self,
        handler: Object,
        env: &Rt<Env>,
        cx: &Context,
    ) -> Result<bool> {
        if matches!(self.error, ErrorType::Throw(_)) {
            return Ok(false);
        }
        if handler.is_nil() {
            return Ok(false);
        }
        if handler == crate::core::object::TRUE {
            return Ok(true);
        }
        if matches!(handler.untag(), ObjectType::Cons(_))
            && condition_any(handler, |item| Ok(item == crate::core::object::TRUE))?
        {
            return Ok(true);
        }
        let condition = match &self.error {
            ErrorType::Signal(exit) => exit.condition(cx).car(),
            ErrorType::Err(error) => error
                .downcast_ref::<LispError>()
                .map_or(sym::ERROR.into(), |e| e.bind(cx).car()),
            ErrorType::Throw(_) => unreachable!(),
        };
        let ObjectType::Symbol(symbol) = condition.untag() else {
            bail!("Signal condition must be a symbol");
        };
        let metadata = env.props.get(symbol).and_then(|properties| {
            properties.iter().find(|p| p.0 == sym::ERROR_CONDITIONS).map(|p| p.1.bind(cx))
        });
        let mut matches = |requested: Object| -> Result<bool> {
            if requested == crate::core::object::TRUE {
                return Ok(true);
            }
            if requested == sym::DEBUG {
                return Ok(false);
            }
            if let Some(metadata) = metadata {
                return condition_any(metadata, |class| Ok(class.ptr_eq(requested)));
            }
            if symbol == sym::QUIT {
                return Ok(requested == sym::QUIT);
            }
            if symbol == sym::ERROR
                || symbol == sym::RUNE_UNIMPLEMENTED
                || matches!(self.error, ErrorType::Err(_))
            {
                return Ok(requested == symbol || requested == sym::ERROR);
            }
            Ok(false)
        };
        match handler.untag() {
            ObjectType::Symbol(_) => matches(handler),
            _ => condition_any(handler, &mut matches),
        }
    }

    pub(crate) fn is_quit(&self, cx: &Context) -> bool {
        matches!(&self.error, ErrorType::Signal(payload) if payload.condition(cx).car() == sym::QUIT)
    }

    pub(crate) fn signal(error_symbol: Object, data: Object, cx: &Context) -> Self {
        Self {
            backtrace: Vec::new(),
            error: ErrorType::Signal(PendingExit::new(error_symbol, data, cx)),
        }
    }

    pub(crate) fn throw(tag: Object, data: Object, cx: &Context) -> Self {
        Self { backtrace: Vec::new(), error: ErrorType::Throw(PendingExit::new(tag, data, cx)) }
    }

    pub(crate) fn new(error: impl Into<Self>) -> Self {
        error.into()
    }

    pub(crate) fn with_trace(error: anyhow::Error, name: &str, args: &[Rto<Object>]) -> Self {
        let trace = diagnostic_trace(name, args);
        Self { backtrace: vec![trace], error: ErrorType::Err(error) }
    }

    pub(crate) fn add_trace(mut self, name: &str, args: &[Rto<Object>]) -> Self {
        self.backtrace.push(diagnostic_trace(name, args));
        self
    }

    pub(crate) fn print_backtrace(&self) {
        println!("BEGIN_BACKTRACE");
        for (i, x) in self.backtrace.iter().enumerate() {
            println!("{i}: {x}");
        }
        println!("END_BACKTRACE");
    }
}

impl From<anyhow::Error> for EvalError {
    fn from(e: anyhow::Error) -> Self {
        Self::new_error(e)
    }
}

impl From<ConsError> for EvalError {
    fn from(e: ConsError) -> Self {
        Self::new_error(anyhow::anyhow!(e))
    }
}

impl From<String> for EvalError {
    fn from(e: String) -> Self {
        Self::new_error(anyhow::anyhow!(e))
    }
}

impl From<&'static str> for EvalError {
    fn from(e: &'static str) -> Self {
        Self::new_error(anyhow::anyhow!(e))
    }
}

impl From<TypeError> for EvalError {
    fn from(e: TypeError) -> Self {
        Self::new_error(e.into())
    }
}

impl From<LispError> for EvalError {
    fn from(e: LispError) -> Self {
        Self::new_error(e.into())
    }
}

impl From<std::convert::Infallible> for EvalError {
    fn from(e: std::convert::Infallible) -> Self {
        Self::new_error(e.into())
    }
}

pub(crate) type EvalResult<'ob> = Result<Object<'ob>, EvalError>;

impl<'new> WithLifetime<'new> for EvalResult<'_> {
    type Out = EvalResult<'new>;

    unsafe fn with_lifetime(self) -> Self::Out {
        self.map(|x| x.with_lifetime())
    }
}

#[defun]
pub(crate) fn apply<'ob>(
    function: &Rto<Function>,
    arguments: ArgSlice,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    // Stage without mutating the caller stack. Improper lists, count overflow,
    // and reservation failures cannot leave a partially appended call behind.
    root!(staged, new(Vec), cx);
    let arguments = env.stack.arg_slice(arguments);
    if let Some((tail, prefix)) = arguments.split_last() {
        ensure!(
            prefix.len() <= usize::from(u16::MAX),
            "Call argument count exceeds supported range"
        );
        staged.bind_mut(cx).try_reserve(prefix.len())?;
        for argument in prefix {
            staged.push(argument.bind(cx));
        }
        for element in tail.bind(cx).as_list()? {
            ensure!(
                staged.len() < usize::from(u16::MAX),
                "Call argument count exceeds supported range"
            );
            staged.bind_mut(cx).try_reserve(1)?;
            staged.push(element?);
        }
    }
    let mut frame = CallFrame::new(env);
    frame.push_arg_slice(Rt::bind_slice(staged, cx));
    function.call(&mut frame, None, cx).map_err(Into::into)
}

#[defun]
pub(crate) fn funcall<'ob>(
    function: &Rto<Function>,
    arguments: ArgSlice,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    let count = arguments.len();
    ensure!(count <= usize::from(u16::MAX), "Call argument count exceeds supported range");
    root!(staged, new(Vec), cx);
    staged.bind_mut(cx).try_reserve(count)?;
    for argument in env.stack.arg_slice(arguments) {
        staged.push(argument.bind(cx));
    }
    let mut frame = CallFrame::new(env);
    frame.push_arg_slice(Rt::bind_slice(staged, cx));
    function.call(&mut frame, None, cx).map_err(Into::into)
}

#[defun]
fn run_hooks<'ob>(hooks: ArgSlice, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    let hook_count = hooks.len();
    for i in 0..hook_count {
        let hook = env.stack[hook_count - i - 1].bind(cx);
        match hook.untag() {
            ObjectType::Symbol(sym) => {
                if let Some(val) = env.vars.get(sym) {
                    let val = val.bind(cx);
                    match val.untag() {
                        ObjectType::Cons(hook_list) => {
                            rooted_iter!(hooks, hook_list, cx);
                            while let Some(hook) = hooks.next()? {
                                let func = hook.try_as()?;
                                call!(func; env, cx)?;
                            }
                        }
                        ObjectType::NIL => {}
                        _ => {
                            let func: Function = val.try_into()?;
                            root!(func, cx);
                            call!(func; env, cx)?;
                        }
                    }
                }
            }
            x => bail!(TypeError::new(Type::Symbol, x)),
        }
    }
    Ok(NIL)
}

#[defun]
fn run_hook_with_args<'ob>(
    hook: &Rto<Object>,
    args: ArgSlice,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    match hook.untag(cx) {
        ObjectType::Symbol(sym) => {
            if let Some(val) = env.vars.get(sym) {
                let val = val.bind(cx);
                match val.untag() {
                    ObjectType::Cons(hook_list) => {
                        rooted_iter!(hooks, hook_list, cx);
                        while let Some(hook) = hooks.next()? {
                            let func: &Rto<Function> = hook.try_as()?;
                            root!(staged, new(Vec), cx);
                            staged.bind_mut(cx).try_reserve(args.len())?;
                            for argument in env.stack.arg_slice(args) {
                                staged.push(argument.bind(cx));
                            }
                            let mut frame = CallFrame::new(env);
                            frame.push_arg_slice(Rt::bind_slice(staged, cx));
                            func.call(&mut frame, None, cx)?;
                        }
                    }
                    ObjectType::NIL => {}
                    _ => {
                        let func: Function = val.try_into()?;
                        root!(func, cx);
                        call!(func; env, cx)?;
                    }
                }
            }
        }
        x => bail!(TypeError::new(Type::Symbol, x)),
    }
    Ok(NIL)
}

#[defun]
pub(crate) fn autoload_do_load<'ob>(
    fundef: &Rto<Object>,
    funname: Option<&Rto<Gc<Symbol>>>,
    macro_only: Option<&Rto<Object>>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    // TODO: want to handle the case where the file is already loaded.
    let Ok((sym::AUTOLOAD, ObjectType::Cons(body))) = fundef.bind(cx).as_cons_pair() else {
        return Ok(fundef.bind(cx));
    };
    ensure!(macro_only.is_none(), "autoload-do-load macro-only is not yet implemented");
    let mut iter = body.elements();
    let file: Gc<&LispString> = match iter.next() {
        Some(x) => x?.try_into()?,
        None => bail!("Malformed autoload"),
    };
    ensure!(
        iter.fallible().all(|x| Ok(x.is_nil()))?,
        "autoload arguments are not yet implemented"
    );
    root!(file, cx);
    crate::lread::load(file, None, None, cx, env)?;
    match funname {
        Some(func) => match func.untag(cx).func(cx) {
            Some(x) => Ok(x.into()),
            None => Err(anyhow!("autoload of {func} did not provide a definition")),
        },
        _ => Ok(NIL),
    }
}

#[defun]
fn autoload<'ob>(
    function: Symbol<'ob>,
    file: &str,
    docstring: Option<Object>,
    interactive: Option<Object>,
    load_type: Option<Object>,
    cx: &'ob Context,
) -> Result<Symbol<'ob>> {
    if function.has_func() {
        Ok(sym::NIL)
    } else {
        let autoload = list![sym::AUTOLOAD, file, docstring, interactive, load_type; cx];
        crate::data::fset(function, autoload)
    }
}

#[defun]
pub(crate) fn macroexpand<'ob>(
    form: &Rto<Object>,
    environment: Option<&Rto<Object>>,
    cx: &'ob mut Context,
    env: &mut Rt<Env>,
) -> Result<Object<'ob>> {
    let ObjectType::Cons(cons) = form.untag(cx) else { return Ok(form.bind(cx)) };
    let ObjectType::Symbol(sym) = cons.car().untag() else { return Ok(form.bind(cx)) };
    // shadow the macro based on ENVIRONMENT
    let func = match environment {
        Some(env) => match assq(sym.into(), env.bind(cx).try_into()?)?.untag() {
            ObjectType::Cons(cons) => Some(cons.cdr().try_into()?),
            _ => get_macro_func(sym, cx),
        },
        _ => get_macro_func(sym, cx),
    };
    let Some(macro_func) = func else { return Ok(form.bind(cx)) };
    let mut iter = cons.cdr().as_list()?.fallible();
    let mut frame = CallFrame::new(env);
    while let Some(arg) = iter.next()? {
        frame.push_arg(arg);
    }
    root!(macro_func, cx);
    let name = sym.name().to_owned();
    let new_form = macro_func.call(&mut frame, Some(&name), cx)?;
    drop(frame);
    root!(new_form, cx); // polonius
    if eq(new_form.bind(cx), form.bind(cx)) {
        Ok(form.bind(cx))
    } else {
        // recursively expand the macro's
        macroexpand(new_form, environment, cx, env)
    }
}

fn get_macro_func<'ob>(name: Symbol, cx: &'ob Context) -> Option<Function<'ob>> {
    if let Some(callable) = name.follow_indirect(cx)
        && let Ok((sym::MACRO, cdr)) = callable.as_cons_pair()
    {
        return Some(cdr.tag());
    }
    None
}

#[defun]
fn func_arity<'ob>(function: Function, cx: &'ob Context) -> Result<&'ob Cons> {
    let from_args = |args: FnArgs| {
        let min = args.required;
        if args.rest {
            // TODO: Handle unevalled
            Cons::new(min, sym::MANY, cx)
        } else {
            Cons::new(min, args.optional + min, cx)
        }
    };
    match function.untag() {
        FunctionType::ByteFn(func) => Ok(from_args(func.args)),
        FunctionType::SubrFn(func) => Ok(from_args(func.args)),
        FunctionType::Cons(func) => {
            let arg_pos = match func.car().untag() {
                ObjectType::Symbol(sym::CLOSURE) => 2,
                ObjectType::Symbol(sym::LAMBDA) => 1,
                other => bail!(TypeError::new(Type::Func, other)),
            };
            let Some(args) = func.elements().fallible().nth(arg_pos)? else {
                bail!("Invalid function: {func}")
            };
            let (req, opt, rest) = crate::interpreter::parse_arg_list(args)?;
            let args = FnArgs {
                required: req.len() as u16,
                optional: opt.len() as u16,
                rest: rest.is_some(),
                ..FnArgs::default()
            };
            Ok(from_args(args))
        }
        FunctionType::Symbol(sym) => {
            let Some(func) = sym.follow_indirect(cx) else { bail!("Void Function: {sym}") };
            func_arity(func, cx)
        }
    }
}

#[defun]
#[expect(non_snake_case)]
fn internal__define_uninitialized_variable<'ob>(
    _symbol: Symbol<'ob>,
    _doc: Option<Object>,
) -> Object<'ob> {
    // TODO: implement doc strings
    NIL
}

/// Shared nonlocal-exit semantics for evaluated native arguments.
pub(crate) fn throw_value(
    tag: Object,
    value: Object,
    env: &mut Rt<Env>,
    cx: &Context,
) -> EvalError {
    if env.catch_stack.iter().any(|entry| entry.bind(cx).ptr_eq(tag)) {
        EvalError::throw(tag, value, cx)
    } else {
        EvalError::new_error(anyhow!("No catch for {}", diagnostic_object(tag)))
    }
}

#[defun]
fn throw(tag: Object, value: Object, env: &mut Rt<Env>, cx: &Context) -> Result<bool> {
    Err(throw_value(tag, value, env, cx).into())
}

#[defun]
fn signal(mut error_symbol: Object, data: Object, cx: &Context) -> Result<bool> {
    if error_symbol.is_nil() && data.is_nil() {
        error_symbol = sym::ERROR.into();
    }
    Err(EvalError::signal(error_symbol, data, cx).into())
}

#[defun]
fn special_variable_p(symbol: Symbol) -> bool {
    symbol.is_special()
}

#[defun]
fn set_default_toplevel_value<'ob>(
    symbol: Symbol,
    value: Object,
    env: &'ob mut Rt<Env>,
) -> Result<Object<'ob>> {
    env.set_var(symbol, value)?;
    Ok(NIL)
}

#[defun]
fn set_default<'ob>(
    symbol: Symbol,
    value: Object<'ob>,
    env: &'ob mut Rt<Env>,
) -> Result<Object<'ob>> {
    // TODO: implement buffer local variables
    env.set_var(symbol, value)?;
    Ok(value)
}

impl Rto<Function<'_>> {
    pub(crate) fn call<'ob>(
        &self,
        frame: &mut CallFrame<'_, '_>,
        name: Option<&str>,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        debug!("calling: {self}");
        let name = name.unwrap_or("lambda");
        frame.finalize_arguments()?;
        let arg_cnt = frame.arg_count();
        cx.garbage_collect(false);
        match self.untag(cx) {
            FunctionType::ByteFn(f) => {
                root!(f, cx);
                crate::bytecode::call(f, arg_cnt, name, frame, cx)
                    .map_err(|e| e.add_trace(name, frame.arg_slice()))
            }
            FunctionType::SubrFn(f) => {
                (*f).call(arg_cnt, frame, cx).map_err(|e| add_trace(e, name, frame.arg_slice()))
            }
            FunctionType::Cons(_) => {
                crate::interpreter::call_closure(self.try_as().unwrap(), arg_cnt, name, frame, cx)
                    .map_err(|e| e.add_trace(name, frame.arg_slice()))
            }
            FunctionType::Symbol(sym) => {
                let Some(func) = sym.follow_indirect(cx) else { bail_err!("Void Function: {sym}") };
                if let Ok((sym::AUTOLOAD, _)) = func.as_cons_pair() {
                    // TODO: inifinite loop if autoload does not resolve
                    root!(sym, cx);
                    crate::eval::autoload_do_load(self.cast(), None, None, frame, cx)
                        .map_err(|e| add_trace(e, name, frame.arg_slice()))?;
                    let Some(func) = sym.bind(cx).follow_indirect(cx) else {
                        bail_err!("autoload for {sym} failed to define function")
                    };
                    root!(func, cx);
                    let name = sym.bind(cx).name().to_owned();
                    func.call(frame, Some(&name), cx)
                } else {
                    root!(func, cx);
                    let name = sym.name().to_owned();
                    func.call(frame, Some(&name), cx)
                }
            }
        }
    }
}

pub(crate) fn add_trace(err: anyhow::Error, name: &str, args: &[Rto<Object>]) -> EvalError {
    match err.downcast::<EvalError>() {
        Ok(err) => err.add_trace(name, args),
        Err(e) => EvalError::with_trace(e, name, args),
    }
}

defsym!(FUNCTION);
defsym!(QUOTE);
defsym!(MACRO);
defsym!(UNQUOTE, ",");
defsym!(SPLICE, ",@");
defsym!(BACKQUOTE, "`");
defsym!(AND_OPTIONAL, "&optional");
defsym!(AND_REST, "&rest");
defsym!(LAMBDA);
defsym!(CLOSURE);
defsym!(CONDITION_CASE);
defsym!(UNWIND_PROTECT);
defsym!(SAVE_EXCURSION);
defsym!(SAVE_CURRENT_BUFFER);
defsym!(WHILE);
defsym!(INLINE);
defsym!(PROGN);
defsym!(PROG1);
defsym!(PROG2);
defsym!(SETQ);
defsym!(DEFCONST);
defsym!(COND);
defsym!(LET);
defsym!(LET_STAR, "let*");
defsym!(IF);
defsym!(AND);
defsym!(OR);
defsym!(INTERACTIVE);
defsym!(CATCH);

defsym!(ERROR);
defsym!(DEBUG);
defsym!(VOID_VARIABLE);

defvar!(DEBUG_ON_ERROR, false);
defvar!(INTERNAL_MAKE_INTERPRETED_CLOSURE_FUNCTION);

#[cfg(test)]
mod pending_exit_tests {
    use super::*;
    use crate::core::{gc::RootSet, object::IntoObject};

    #[test]
    fn pending_exit_keeps_original_identity_across_other_exits_and_collections() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let tag = Cons::new1(17, cx);
        root!(tag, cx);
        let value = vec![Object::from(7)].into_obj(&cx.block);
        root!(value, cx);
        let error = EvalError::throw(tag.bind(cx).into(), value.bind(cx).into(), cx);
        for _ in 0..5 {
            let _other = EvalError::signal(sym::ERROR.into(), NIL, cx);
            cx.garbage_collect(true);
            let ErrorType::Throw(payload) = &error.error else { panic!("expected throw") };
            let pair = payload.bind(cx).unwrap();
            assert!(pair.car().ptr_eq(Object::from(tag.bind(cx))));
            assert!(pair.cdr().ptr_eq(Object::from(value.bind(cx))));
        }
    }

    #[test]
    fn constructing_pending_exit_does_not_render_recursive_payload() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let cycle = Cons::new1(17, cx);
        cycle.set_cdr(cycle.into()).unwrap();
        let error = EvalError::throw(Object::from(17), cycle.into(), cx);
        for _ in 0..5 {
            cx.garbage_collect(true);
        }
        let ErrorType::Throw(payload) = &error.error else { panic!("expected throw") };
        let pair = payload.bind(cx).unwrap();
        let cycle = pair.cdr().cons().unwrap();
        assert!(cycle.cdr().ptr_eq(pair.cdr()));
    }

    #[test]
    fn live_foreign_binding_fails_without_disturbing_owner_payload() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let tag = Cons::new1(17, cx);
        root!(tag, cx);
        let error = EvalError::throw(Object::from(tag.bind(cx)), Object::from(7), cx);
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let roots = RootSet::default();
                    let foreign = Context::new(&roots);
                    let ErrorType::Throw(payload) = &error.error else { panic!("expected throw") };
                    assert!(payload.bind(&foreign).is_err());
                    assert_eq!(payload.condition(&foreign).car(), sym::ERROR);
                })
                .join()
                .unwrap();
        });
        cx.garbage_collect(true);
        let ErrorType::Throw(payload) = &error.error else { panic!("expected throw") };
        let pair = payload.bind(cx).unwrap();
        assert!(pair.car().ptr_eq(Object::from(tag.bind(cx))));
        assert_eq!(pair.cdr(), Object::from(7));
    }

    #[test]
    fn retired_and_foreign_payload_never_recreates_catch_identity() {
        let error = {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            EvalError::throw(cx.add("owner-tag"), Object::from(7), &cx)
        };
        std::thread::spawn(move || {
            let roots = RootSet::default();
            let cx = Context::new(&roots);
            let ErrorType::Throw(payload) = &error.error else { panic!("expected throw") };
            assert!(payload.bind(&cx).is_err());
            let fallback = payload.condition(&cx);
            assert_eq!(fallback.car(), sym::ERROR);
            assert!(matches!(fallback.cdr().untag(), ObjectType::String(_)));
        })
        .join()
        .unwrap();
    }
}

#[cfg(test)]
mod exit_diagnostic_tests {
    use super::*;
    use crate::core::{gc::RootSet, object::IntoObject};

    #[test]
    fn diagnostics_do_not_traverse_self_referential_bytecode_constants() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        let constants = vec![NIL].into_obj(&cx.block);
        root!(constants, cx);
        let codes = vec![135u8].into_obj(&cx.block);
        let function = crate::alloc::make_byte_code(
            0,
            codes.untag(),
            constants.bind(cx).untag(),
            1,
            None,
            None,
            &[],
            cx,
        )
        .unwrap();
        root!(function, cx);
        constants.bind(cx).untag().try_mut().unwrap()[0].set(function.bind(cx).into());
        root!(env, init(Env::new(cx).unwrap()), cx);
        let invalid = EvalError::signal(function.bind(cx).into(), NIL, cx);
        let error = invalid.matches_handler(sym::ERROR.into(), env, cx).unwrap_err();
        assert_eq!(error.to_string(), "Signal condition must be a symbol");
        let mut frame = CallFrame::new(env);
        frame.push_arg(Object::from(function.bind(cx)));
        frame.finalize_arguments().unwrap();
        let throwing = EvalError::throw(Object::from(17), function.bind(cx).into(), cx)
            .add_trace("throw", frame.arg_slice());
        assert_eq!(&*throwing.backtrace[0], "throw [<Func>]");
        let ErrorType::Throw(payload) = &throwing.error else { panic!("expected throw") };
        assert!(payload.bind(cx).unwrap().cdr().ptr_eq(Object::from(function.bind(cx))));
        let signal = EvalError::signal(sym::ERROR.into(), function.bind(cx).into(), cx)
            .add_trace("signal", frame.arg_slice());
        assert_eq!(&*signal.backtrace[0], "signal [<Func>]");
        let host = EvalError::with_trace(anyhow!("host diagnostic"), "host", frame.arg_slice());
        assert_eq!(&*host.backtrace[0], "host [<Func>]");
        let unmatched = throw_value(function.bind(cx).into(), NIL, &mut frame, cx);
        assert!(unmatched.to_string().contains("No catch for <Func>"));
    }

    #[test]
    fn argument_and_text_diagnostics_are_bounded_and_escape_control_characters() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        let mut frame = CallFrame::new(env);
        let text = "界\n".repeat(1000);
        for _ in 0..100 {
            frame.push_arg(cx.add(text.as_str()));
        }
        frame.finalize_arguments().unwrap();
        let error = EvalError::with_trace(
            anyhow!("host diagnostic"),
            &"name".repeat(1000),
            frame.arg_slice(),
        );
        let trace = &error.backtrace[0];
        assert!(trace.len() < 12_000);
        assert!(trace.contains('…'));
        assert!(trace.contains("\\n"));
        assert!(!trace.contains('\n'));
        assert_eq!(trace.matches('"').count(), DIAGNOSTIC_ARGUMENTS * 2);
    }
}

#[cfg(test)]
mod checked_argument_tests {
    use super::*;
    use crate::core::gc::RootSet;

    #[test]
    fn checked_arguments_reject_oversized_apply_and_reuse_original_environment() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        let list = crate::fns::slice_into_list(&vec![NIL; usize::from(u16::MAX) + 1], None, cx);
        root!(list, cx);
        let function: Function = crate::core::env::intern("list", cx).into();
        root!(function, cx);
        for _ in 0..3 {
            let mut frame = CallFrame::new(env);
            frame.push_arg(Object::from(list.bind(cx)));
            frame.finalize_arguments().unwrap();
            let before = frame.stack.len();
            let depth = frame.stack.current_frame();
            let error = apply(function, ArgSlice::new(1), &mut frame, cx).unwrap_err();
            assert!(error.to_string().contains("argument count exceeds supported range"));
            assert_eq!(frame.stack.len(), before);
            assert_eq!(frame.stack.current_frame(), depth);
            assert_eq!(frame.arg_count(), 1);
        }
    }

    #[test]
    fn checked_arguments_maximum_apply_count_is_accepted() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        let list =
            crate::fns::slice_into_list(&vec![Object::from(0); usize::from(u16::MAX)], None, cx);
        root!(list, cx);
        let function: Function = crate::core::env::intern("+", cx).into();
        root!(function, cx);
        let mut frame = CallFrame::new(env);
        frame.push_arg(Object::from(list.bind(cx)));
        frame.finalize_arguments().unwrap();
        assert_eq!(apply(function, ArgSlice::new(1), &mut frame, cx).unwrap(), Object::from(0));
        assert_eq!(frame.arg_count(), 1);
    }

    #[test]
    fn checked_arguments_improper_apply_tail_does_not_append_partial_arguments() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        let function: Function = crate::core::env::intern("list", cx).into();
        root!(function, cx);
        let improper = Cons::new(Object::from(7), Object::from(9), cx);
        root!(improper, cx);
        let mut frame = CallFrame::new(env);
        frame.push_arg(Object::from(3));
        frame.push_arg(Object::from(improper.bind(cx)));
        frame.finalize_arguments().unwrap();
        let before = frame.stack.len();
        assert!(apply(function, ArgSlice::new(2), &mut frame, cx).is_err());
        assert_eq!(frame.stack.len(), before);
        assert_eq!(frame.arg_count(), 2);
    }

    #[test]
    fn checked_arguments_direct_native_call_overflow_is_a_lisp_error() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        let function: Function = crate::core::env::intern("list", cx).into();
        root!(function, cx);
        {
            let mut frame = CallFrame::new(env);
            for _ in 0..=u16::MAX {
                frame.push_arg(NIL);
            }
            let error = function.call(&mut frame, Some("list"), cx).unwrap_err();
            assert!(error.to_string().contains("argument count exceeds supported range"));
        }
        assert_eq!(env.stack.current_frame(), 0);
        assert_eq!(env.stack.len(), 0);
        let mut frame = CallFrame::new(env);
        let result = function.call(&mut frame, Some("list"), cx).unwrap();
        assert!(result.is_nil());
    }
}

// QUIT is not an ERROR subtype.
defsym!(QUIT);
defvar!(INHIBIT_QUIT);

defsym!(ERROR_CONDITIONS);

defsym!(WRONG_TYPE_ARGUMENT);
defsym!(VOID_FUNCTION);
defsym!(INVALID_FUNCTION);
defsym!(SETTING_CONSTANT);
defsym!(ARITH_ERROR);
defsym!(RANGE_ERROR);
defsym!(OVERFLOW_ERROR);
