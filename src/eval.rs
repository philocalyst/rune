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
                || symbol == sym::VOID_VARIABLE
                || symbol == sym::VOID_FUNCTION
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
        match e.downcast::<Self>() {
            Ok(error) => error,
            Err(error) => Self::new_error(error),
        }
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
        // CallFrame restores the original stack after each callback. Root the
        // current symbol and selected value independently across callback GC.
        let hook = env.stack[hook_count - i - 1].bind(cx);
        root!(hook, cx);
        let symbol: Symbol = hook.bind(cx).try_into()?;
        let value = env.variable_value(symbol, cx)?.unwrap_or(NIL);
        root!(value, cx);
        hook_value(value, hook, ArgSlice::new(0), false, false, None, &mut 65_536, env, cx)?;
    }
    Ok(NIL)
}

#[defun]
fn run_hook_with_args<'ob>(hook: &Rto<Object>, args: ArgSlice,
    env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    let symbol: Symbol = hook.bind(cx).try_into()?;
    let value = env.variable_value(symbol, cx)?.unwrap_or(NIL);
    root!(value, cx);
    hook_value(value, hook, args, false, false, None, &mut 65_536, env, cx)
}

/// Call one hook with the original arguments, retaining them across callbacks.
fn hook_success_call<'ob>(entry: &Rto<Object>, args: ArgSlice, wrapper: Option<&Rto<Object>>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    // In wrapped mode the hook entry is data, not necessarily callable: only
    // the wrapper is invoked, and it may intentionally accept any entry.
    let target: Function = wrapper.map_or(entry.bind(cx), |w| w.bind(cx)).try_into()?;
    root!(target, cx);
    let count = args.len().checked_add(usize::from(wrapper.is_some()))
        .ok_or_else(|| anyhow::anyhow!("Hook argument count overflow"))?;
    ensure!(count <= usize::from(u16::MAX), "Hook argument count exceeds supported range");
    root!(staged, new(Vec), cx);
    staged.bind_mut(cx).try_reserve(count)?;
    if wrapper.is_some() { staged.push(entry.bind(cx)); }
    for argument in env.stack.arg_slice(args) { staged.push(argument.bind(cx)); }
    let mut frame = CallFrame::new(env);
    frame.push_arg_slice(Rt::bind_slice(staged, cx));
    target.call(&mut frame, None, cx).map_err(Into::into)
}

fn hook_value<'ob>(value: &Rto<Object>, hook: &Rto<Object>, args: ArgSlice,
    global: bool, until_success: bool, wrapper: Option<&Rto<Object>>, budget: &mut usize, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    if value.bind(cx).is_nil() { return Ok(NIL); }
    let single = match value.untag(cx) {
        ObjectType::Cons(pair) => pair.car() == sym::LAMBDA || pair.car() == sym::CLOSURE,
        _ => true,
    };
    if single {
        let result = hook_success_call(value, args, wrapper, env, cx)?;
        return Ok(if until_success { result } else { NIL });
    }
    root!(tail, value.bind(cx), cx);
    // Transfer callback values into an owner-rooted slot before continuing the
    // loop, so their Context borrow does not escape across later callbacks.
    root!(result, NIL, cx);
    loop {
        let ObjectType::Cons(pair) = tail.untag(cx) else { return Ok(NIL); };
        ensure!(*budget > 0, "Hook traversal limit exceeded");
        *budget -= 1;
        let function = pair.car();
        // Keep the current cons rooted until after the callback: mutation of its
        // cdr during callback is observed, as in GNU's live list traversal.
        if function == sym::TRUE {
            if !global {
                let symbol: Symbol = hook.bind(cx).try_into()?;
                let defaults = crate::data::default_value(symbol, env, cx)?;
                root!(defaults, cx);
                hook_value(defaults, hook, args, true, until_success, wrapper, budget, env, cx)
                    .map(|value| result.set(value))?;
                if until_success && !result.bind(cx).is_nil() { return Ok(result.bind(cx)); }
            }
        } else {
            root!(function, function, cx);
            hook_success_call(function, args, wrapper, env, cx)
                .map(|value| result.set(value))?;
            if until_success && !result.bind(cx).is_nil() { return Ok(result.bind(cx)); }
        }
        // Discard ignored callback returns before the next callback/GC.
        result.set(NIL);
        let ObjectType::Cons(pair) = tail.untag(cx) else { unreachable!() };
        tail.set(pair.cdr());
    }
}

#[defun]
fn run_hook_with_args_until_success<'ob>(hook: &Rto<Object>, args: ArgSlice,
    env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    let symbol: Symbol = hook.bind(cx).try_into()?;
    let value = env.variable_value(symbol, cx)?.unwrap_or(NIL);
    root!(value, cx);
    hook_value(value, hook, args, false, true, None, &mut 65_536, env, cx)
}


#[defun]
fn run_hook_wrapped<'ob>(hook: &Rto<Object>, wrapper: &Rto<Object>, args: ArgSlice,
    env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    let symbol: Symbol = hook.bind(cx).try_into()?;
    let value = env.variable_value(symbol, cx)?.unwrap_or(NIL);
    root!(value, cx);
    hook_value(value, hook, args, false, true, Some(wrapper), &mut 65_536, env, cx)
}


#[cfg(test)]
mod hook_with_args_tests {
    #[test]
    fn unbound_nil_symbol_and_raw_lambda_receive_expected_arguments() {
        crate::interpreter::assert_lisp("(progn (defvar ha-empty) (defvar ha-symbol) (defvar ha-lambda) (defvar ha-seen) (let ((ha-empty nil) (ha-symbol 'list) (ha-lambda '(lambda (a b) (setq ha-seen (list a b)))) (ha-seen nil)) (list (run-hook-with-args 'ha-unbound 1) (run-hook-with-args 'ha-empty 1) (run-hook-with-args 'ha-symbol 3 4) (run-hook-with-args 'ha-lambda 5 6) ha-seen)))", "(nil nil nil nil (5 6))");
    }
    #[test]
    fn closure_keeps_captured_value_and_original_argument_identity() {
        crate::interpreter::assert_lisp("(progn (defvar ha-seen) (defvar ha-value) (defvar ha-hook) (let ((ha-seen nil) (ha-value (list 'value)) (ha-hook '(closure ((captured . 8)) (x) (garbage-collect) (setq ha-seen (list captured x))))) (run-hook-with-args 'ha-hook ha-value) (list (car ha-seen) (eq (car (cdr ha-seen)) ha-value))))", "(8 t)");
    }
    #[test]
    fn list_continues_after_nonnil_with_live_cdr_and_gc() {
        crate::interpreter::assert_lisp("(progn (defvar ha-seen) (defvar ha-hook) (let ((ha-seen nil) (ha-hook nil)) (setq ha-hook (list (function (lambda (x) (setq ha-seen (cons x ha-seen)) (setcdr ha-hook (list (function (lambda (y) (garbage-collect) (setq ha-seen (cons y ha-seen)))))))) (function (lambda (x) (error \"replaced callback\"))))) (list (run-hook-with-args 'ha-hook '(payload)) ha-seen)))", "(nil ((payload) (payload)))");
    }
    #[test]
    fn errors_and_throws_stop_iteration_and_next_call_recovers() {
        crate::interpreter::assert_lisp("(progn (defvar ha-hook) (defvar ha-result) (progn (fset 'ha-thrower '(lambda (x) (garbage-collect) (throw 'ha-exit x))) (let ((ha-hook (list 'ha-thrower (function (lambda (x) (error \"unreachable\"))))) (ha-result nil)) (setq ha-result (catch 'ha-exit (run-hook-with-args 'ha-hook 7))) (setq ha-hook (list (function (lambda (x) (error \"expected\"))))) (let ((caught (condition-case nil (run-hook-with-args 'ha-hook 8) (error t)))) (setq ha-hook 'list) (list ha-result caught (backtrace-frame--internal #'list 0 'ha-thrower) (run-hook-with-args 'ha-hook 9))))))", "(7 t nil nil)");
    }
    #[test]
    fn ordinary_hook_side_effects_match_gnu_batch() {
        for form in [
            "(let ((ha-hook nil)) (run-hook-with-args 'ha-hook 1))",
            "(let ((ha-hook 'list)) (run-hook-with-args 'ha-hook 3 4))",
            "(let ((ha-seen nil) (ha-hook '(lambda (a b) (setq ha-seen (list a b))))) (run-hook-with-args 'ha-hook 5 6) ha-seen)",
            "(let ((ha-seen nil) (ha-hook (list (lambda (x) (setq ha-seen (cons x ha-seen)) t) (lambda (x) (garbage-collect) (setq ha-seen (cons x ha-seen)))))) (run-hook-with-args 'ha-hook '(payload)) ha-seen)",
            "(let ((ha-hook (list (lambda (x) (throw 'ha-exit x))))) (catch 'ha-exit (run-hook-with-args 'ha-hook 7)))",
        ] { crate::assert_elprop!("{form}"); }
    }
}

#[cfg(test)]
mod hook_until_success_tests {
    #[test]
    fn recursive_hook_reentry_preserves_outer_arguments_across_gc() {
        crate::interpreter::assert_lisp("(progn (defvar hu-nested) (defvar hu-hooks) (let ((hu-nested nil) (hu-hooks (list (function (lambda (n payload) (if (= n 0) nil (setq hu-nested (run-hook-with-args-until-success 'hu-hooks 0 '(inner)))) (garbage-collect) nil)) (function (lambda (n payload) (list n payload)))))) (let ((result (run-hook-with-args-until-success 'hu-hooks 1 '(outer)))) (list hu-nested result))))", "((0 (inner)) (1 (outer)))");
    }

    #[test]
    fn recursive_hook_reentry_preserves_heap_arguments_across_gc() {
        crate::interpreter::assert_lisp("(progn (defvar hu-payload) (defvar hu-hooks) (let ((hu-payload (list 'p 'q)) (hu-hooks nil)) (setq hu-hooks (list (function (lambda (payload depth) (garbage-collect) (if (= depth 0) (list 'inner payload) (let ((nested (run-hook-with-args-until-success 'hu-hooks payload 0))) (garbage-collect) (list 'outer nested payload))))) (function (lambda (&rest args) (error \"must not run\"))))) (let ((answer (run-hook-with-args-until-success 'hu-hooks hu-payload 1))) (garbage-collect) (list (car answer) (car (nth 1 answer)) (eq (nth 1 (nth 1 answer)) hu-payload) (eq (nth 2 answer) hu-payload)))))", "(outer inner t t)");
    }
    #[test]
    fn hook_throw_unwinds_depth_before_following_call() {
        use crate::core::{env::{Env, sym}, gc::{Context, RootSet}};
        use rune_core::macros::{root, rebind};
        let roots = RootSet::default(); let cx = &mut Context::new(&roots);
        sym::init_symbols(); root!(env, init(Env::new(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let form = "(progn (defvar hu-hooks) (progn (fset 'hu-throw-target '(lambda (x) (garbage-collect) (throw 'hu-exit x))) (let ((hu-hooks (list 'hu-throw-target))) (list (catch 'hu-exit (run-hook-with-args-until-success 'hu-hooks 7)) (backtrace-frame--internal #'list 0 'hu-throw-target)))))";
        let result = {
            let object = crate::reader::read(form, cx).unwrap().0; root!(object,cx);
            rebind!(crate::interpreter::eval(object,None,env,cx).unwrap(), cx)
        };
        root!(result,cx);
        assert_eq!(env.backtrace_depth(),0);
        let expected = crate::reader::read("(7 nil)",cx).unwrap().0;
        assert_eq!(result.bind(cx),expected);
        let object = crate::reader::read("(progn (defvar hu-hooks) (let ((hu-hooks (list (function (lambda (x) x))))) (run-hook-with-args-until-success 'hu-hooks 9)))",cx).unwrap().0;
        root!(object,cx);
        let result = crate::interpreter::eval(object,None,env,cx).unwrap();
        assert_eq!(result,crate::core::object::Object::from(9));
        assert_eq!(env.backtrace_depth(),0);
    }
    #[test]
    fn cyclic_hook_reaches_actual_budget_and_recovers() {
        crate::interpreter::assert_lisp("(progn (defvar hu-hooks) (defvar hu-count) (defvar hu-stop) (let ((hu-hooks nil) (hu-count 0) (hu-stop nil)) (setq hu-hooks (list (function (lambda () (setq hu-count (1+ hu-count)) nil)))) (setcdr hu-hooks hu-hooks) (setq hu-stop (condition-case nil (run-hook-with-args-until-success 'hu-hooks) (error 'bounded))) (setq hu-hooks (list (function (lambda () 'recovered)))) (list hu-stop hu-count (run-hook-with-args-until-success 'hu-hooks))))", "(bounded 65536 recovered)");
    }

    #[test]
    fn ordinary_hook_contract_matches_gnu_batch() {
        for form in [
            "(let ((hu-empty nil)) (list (run-hook-with-args-until-success 'hu-unbound 1) (run-hook-with-args-until-success 'hu-empty 1)))",
            "(let ((hu-one '(lambda (a b) (list b a)))) (run-hook-with-args-until-success 'hu-one 3 4))",
            "(let ((hu-hooks (list (lambda (x) nil) (lambda (x) (garbage-collect) (list x)) (lambda (x) (error \"must not run\"))))) (run-hook-with-args-until-success 'hu-hooks '(payload)))",
            "(let ((hu-hooks (list (lambda (x) (throw 'hu-exit x)) (lambda (x) (error \"must not run\"))))) (catch 'hu-exit (run-hook-with-args-until-success 'hu-hooks 7)))",
            "(let ((hu-hooks (list t (lambda () 'answer)))) (run-hook-with-args-until-success 'hu-hooks))",
        ] { crate::assert_elprop!("{form}"); }
    }
    #[test]
    fn nil_unbound_and_all_nil_return_nil() {
        crate::interpreter::assert_lisp("(progn (defvar hu-empty) (defvar hu-all) (let ((hu-empty nil) (hu-all (list (function (lambda (&rest args) nil)) (function (lambda (&rest args) nil))))) (list (run-hook-with-args-until-success 'hu-unbound 1) (run-hook-with-args-until-success 'hu-empty 1) (run-hook-with-args-until-success 'hu-all 1))))", "(nil nil nil)");
    }
    #[test]
    fn single_raw_lambda_gets_all_arguments() {
        crate::interpreter::assert_lisp("(progn (defvar hu-one) (let ((hu-one '(lambda (a b) (list b a)))) (run-hook-with-args-until-success 'hu-one 3 4)))", "(4 3)");
    }
    #[test]
    fn ordered_short_circuit_keeps_return_identity_across_gc() {
        crate::interpreter::assert_lisp("(progn (defvar hu-seen) (defvar hu-return) (defvar hu-hooks) (let ((hu-seen nil) (hu-return (list 'answer)) (hu-hooks (list (function (lambda (x) (setq hu-seen (cons x hu-seen)) (garbage-collect) nil)) (function (lambda (x) (setq hu-seen (cons x hu-seen)) (garbage-collect) hu-return)) (function (lambda (x) (error \"must not run\")))))) (let ((answer (run-hook-with-args-until-success 'hu-hooks '(payload)))) (garbage-collect) (list (eq answer hu-return) hu-seen))))", "(t ((payload) (payload)))");
    }
    #[test]
    fn hook_errors_propagate_and_following_call_still_works() {
        crate::interpreter::assert_lisp("(progn (defvar hu-hooks) (defvar hu-result) (let ((hu-hooks (list (function (lambda (x) (garbage-collect) (error \"hook failure\"))))) (hu-result nil)) (setq hu-result (condition-case nil (run-hook-with-args-until-success 'hu-hooks 7) (error 'caught))) (setq hu-hooks (list (function (lambda (x) x)))) (list hu-result (run-hook-with-args-until-success 'hu-hooks 9))))", "(caught 9)");
    }
    #[test]
    fn live_cdr_mutation_and_global_t_sentinel_are_bounded() {
        crate::interpreter::assert_lisp("(progn (defvar hu-hooks) (let ((hu-hooks nil)) (setq hu-hooks (list (function (lambda () (setcdr hu-hooks (list (function (lambda () 'replacement)))) nil)) (function (lambda () 'old)))) (run-hook-with-args-until-success 'hu-hooks)))", "replacement");
        crate::interpreter::assert_lisp("(progn (defvar hu-hooks) (let ((hu-hooks (list t (function (lambda () 'answer))))) (run-hook-with-args-until-success 'hu-hooks)))", "answer");
    }
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
    // DOCSTRING, INTERACTIVE, and TYPE are stored metadata, not forms to
    // evaluate. Official generated catalogs normally contain non-nil docs.
    // Bound traversal also rejects a malformed/cyclic tail before file I/O.
    let mut metadata_count = 0;
    for metadata in iter {
        metadata?;
        metadata_count += 1;
        ensure!(metadata_count <= 3, "Malformed autoload metadata tail");
    }
    root!(file, cx);
    crate::lread::load(file, None, None, cx, env)?;
    match funname {
        Some(func) => match crate::data::owner_callable(func.untag(cx), env, cx)? {
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
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Symbol<'ob>> {
    if !crate::data::symbol_function_owner(function, env, cx).is_nil() {
        Ok(sym::NIL)
    } else {
        let autoload = list![sym::AUTOLOAD, file, docstring, interactive, load_type; cx];
        crate::data::fset_owner(function, autoload, env, cx)?;
        Ok(function)
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
        Some(lexenv) => match assq(sym.into(), lexenv.bind(cx).try_into()?)?.untag() {
            ObjectType::Cons(cons) => Some(cons.cdr().try_into()?),
            _ => get_macro_func(sym, env, cx)?,
        },
        _ => get_macro_func(sym, env, cx)?,
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

fn get_macro_func<'ob>(name: Symbol, env: &Rt<Env>, cx: &'ob Context) -> Result<Option<Function<'ob>>> {
    if let Some(callable) = crate::data::owner_callable(name, env, cx)?
        && let Ok((sym::MACRO, cdr)) = callable.as_cons_pair()
    {
        return Ok(Some(cdr.tag()));
    }
    Ok(None)
}

#[defun]
fn func_arity<'ob>(function: Function, env: &Rt<Env>, cx: &'ob Context) -> Result<&'ob Cons> {
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
            let Some(func) = crate::data::owner_callable(sym, env, cx)? else { bail!("Void Function: {sym}") };
            func_arity(func, env, cx)
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
fn set_default<'ob>(
    symbol: Symbol,
    value: Object<'ob>,
    env: &'ob mut Rt<Env>,
    cx: &Context,
) -> Result<Object<'ob>> {
    // Ordinary variables share their current dynamic value; generic buffer
    // local defaults other than the explicit owner forwarder remain unsupported.
    env.set_var_context(symbol, value, cx)?;
    Ok(value)
}

impl Rto<Function<'_>> {
    pub(crate) fn call<'ob>(
        &self,
        frame: &mut CallFrame<'_, '_>,
        name: Option<&str>,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        // Never recursively format a function graph: closures can capture
        // themselves or cyclic owner objects even on a successful call.
        debug!("calling function");
        let name = name.unwrap_or("lambda");
        frame.finalize_arguments()?;
        let checkpoint = frame.backtrace_depth();
        let inherited = frame.backtrace_registered();
        frame.register_backtrace(self.bind(cx).into(), cx);
        let outcome = self.call_registered(frame,Some(name),cx);
        if !inherited {
            frame.backtrace_truncate(checkpoint);
            frame.clear_backtrace_registration();
        }
        outcome
    }

    fn call_registered<'ob>(&self, frame: &mut CallFrame<'_, '_>, name: Option<&str>, cx: &'ob mut Context) -> EvalResult<'ob> {
        let name = name.unwrap_or("lambda");
        let arg_cnt = frame.arg_count();
        cx.garbage_collect(false);
        match self.untag(cx) {
            FunctionType::ByteFn(f) => {
                root!(f, cx);
                crate::bytecode::call(f, arg_cnt, name, frame, cx)
                    .map_err(|e| e.add_trace(name, frame.arg_slice()))
            }
            FunctionType::SubrFn(f) => {
                match (*f).call(arg_cnt, frame, cx) {
                    Ok(value) => Ok(rune_core::macros::rebind!(value, cx)),
                    Err(error) => {
                        if error.downcast_ref::<rune_editor::EditorError>()
                            .is_some_and(|error| matches!(error, rune_editor::EditorError::ReadOnly))
                        {
                            Err(EvalError::signal(sym::BUFFER_READ_ONLY.into(), NIL, cx)
                                .add_trace(name, frame.arg_slice()))
                        } else { Err(add_trace(error, name, frame.arg_slice())) }
                    }
                }
            }
            FunctionType::Cons(_) => {
                crate::interpreter::call_closure(self.try_as().unwrap(), arg_cnt, name, frame, cx)
                    .map_err(|e| e.add_trace(name, frame.arg_slice()))
            }
            FunctionType::Symbol(sym) => {
                let Some(func) = crate::data::owner_callable(sym, frame, cx)? else { bail_err!("Void Function: {sym}") };
                root!(sym, cx);
                root!(func, cx);
                let mut autoloads = 0usize;
                while matches!(func.bind(cx).as_cons_pair(), Ok((sym::AUTOLOAD, _))) {
                    if autoloads == 64 { bail_err!("Autoload chain exceeds 64 loads") }
                    autoloads += 1;
                    crate::eval::autoload_do_load(func.cast(), None, None, frame, cx)
                        .map_err(|e| add_trace(e, name, frame.arg_slice()))?;
                    let Some(loaded) = crate::data::owner_callable(sym.bind(cx), frame, cx)? else {
                        bail_err!("autoload failed to define function")
                    };
                    if Object::from(loaded).ptr_eq(Object::from(func.bind(cx))) {
                        bail_err!("autoload did not change function definition")
                    }
                    func.set(loaded);
                }
                let name = sym.bind(cx).name().to_owned();
                func.call(frame, Some(&name), cx)
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
// GNU startup.el declares this object-valued startup flag nil. Rune owns its
// startup driver; no interactive Lisp debugger is fabricated by this flag.
defvar!(INIT_FILE_DEBUG, false);
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

#[cfg(test)]
mod startup_debug_state_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn init_file_debug_has_gnu_default_and_real_dynamic_lisp_value() {
        let source = "(list (boundp 'init-file-debug) init-file-debug (let ((init-file-debug t)) init-file-debug) init-file-debug)";
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let form = crate::reader::read(source, cx).unwrap().0;
        root!(form, cx);
        assert_eq!(
            format!("{}", crate::interpreter::eval(form, None, env, cx).unwrap()),
            "(t nil t nil)"
        );
        crate::library::elprop::start_emacs().unwrap();
        assert_eq!(crate::library::elprop::eval(source).unwrap(), "(t nil t nil)");
    }
}

#[cfg(test)]
mod autoload_metadata_tests {
    use super::*;
    use crate::core::gc::RootSet;
    #[test]
    fn documented_autoload_loads_real_source_without_evaluating_metadata() {
        let directory = std::env::temp_dir().join(format!("rune-autoload-metadata-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("documented.el");
        std::fs::write(&file, "(fset 'rune-documented-autoload '(lambda () 42))").unwrap();
        sym::init_symbols();
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        let metadata = list![sym::AUTOLOAD, file.to_str().unwrap(), "Official-style documentation", true, NIL; cx];
        root!(metadata, cx);
        autoload_do_load(metadata, None, None, env, cx).unwrap();
        let loaded: Object = crate::data::symbol_function_owner(crate::core::env::intern("rune-documented-autoload", cx), env, cx);
        assert!(!loaded.is_nil());
        assert_ne!(loaded.as_cons_pair().unwrap().0, sym::AUTOLOAD);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn malformed_autoload_metadata_rejects_before_io() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        let malformed = list![sym::AUTOLOAD, "/nonexistent/rune-autoload", NIL, NIL, NIL, true; cx];
        root!(malformed, cx);
        let error = autoload_do_load(malformed, None, None, env, cx).unwrap_err();
        assert_eq!(error.to_string(), "Malformed autoload metadata tail");
    }

}

/// A distinct native identity is the interactive marker inspected by subr.el.
#[defun]
pub(crate) fn funcall_interactively<'ob>(function: &Rto<Function>, arguments: ArgSlice,
    env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    funcall(function, arguments, env, cx)
}

// Special forms dispatch directly in this interpreter rather than through a
// native function cell. Preserve their canonical designator for base matching;
// otherwise an undefined BASE would spuriously match every nil function cell.
fn backtrace_identity<'ob>(mut function: Object<'ob>, env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    let mut seen = Vec::new();
    for _ in 0..256 {
        let ObjectType::Symbol(symbol) = function.untag() else { return Ok(function) };
        if function.is_nil() { return Ok(function); }
        if matches!(symbol, sym::QUOTE | sym::LET | sym::LET_STAR | sym::IF | sym::AND | sym::OR |
            sym::COND | sym::WHILE | sym::PROGN | sym::INLINE | sym::PROG1 | sym::PROG2 |
            sym::SETQ | sym::DEFVAR | sym::DEFCONST | sym::FUNCTION | sym::INTERACTIVE |
            sym::CATCH | sym::CONDITION_CASE | sym::SAVE_CURRENT_BUFFER | sym::SAVE_EXCURSION |
            sym::UNWIND_PROTECT) { return Ok(function); }
        if seen.iter().any(|old: &Object| old.ptr_eq(function)) {
            return crate::data::resolve_owner_function(function,env,cx);
        }
        seen.push(function);
        function = crate::data::symbol_function_owner(symbol,env,cx);
    }
    bail!("Function indirection limit exceeded")
}

/// Copy fixed membership before any user callback can extend the active stack.
fn backtrace_snapshot<'ob>(base: Object<'ob>, env: &Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    // GNU decides whether to search before unpacking (OFFSET . FUNCTION).
    // A cons with nil cdr still searches for nil; only plain nil means top.
    let search_required = !base.is_nil();
    let (offset, base) = match base.untag() {
        ObjectType::Cons(pair) => match pair.car().untag() {
            ObjectType::Int(offset) => {
                // GNU get_backtrace_starting_at stores XFIXNUM in C int.
                // Preserve that ABI narrowing before the positive skip test.
                // Observed on our pinned Linux GNU30.1 (32-bit C int); this
                // does not widen Rune fixnums or claim other-platform parity.
                let offset = offset as std::os::raw::c_int;
                (offset.max(0) as usize, pair.cdr())
            },
            _ => (0,base),
        },
        _ => (0,base),
    };
    let target = if search_required { Some(backtrace_identity(base,env,cx)?) } else { None };
    let mut found = target.is_none();
    let mut skip = offset;
    let mut frames = Vec::new();
    for entry in env.active_backtrace.iter().rev() {
        let [evaluated,function,args,flags] = crate::core::env::backtrace_components(entry.bind(cx))?;
        if !found {
            found = backtrace_identity(function,env,cx)?.ptr_eq(target.unwrap());
        }
        if !found { continue; }
        if skip > 0 { skip-=1; continue; }
        let args = if !evaluated.is_nil() {
            let args: Vec<_> = args.as_list()?.collect::<Result<Vec<_>,_>>()?;
            crate::fns::slice_into_list(&args,None,cx)
        } else { args };
        frames.push(list![evaluated,function,args,flags;cx]);
    }
    Ok(crate::fns::slice_into_list(&frames,None,cx))
}

#[defun(name = "backtrace-frame--internal")]
fn backtrace_frame_internal<'ob>(callback: &Rto<Object>, nframes: &Rto<Object>, base: &Rto<Object>,
    env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    let index = match nframes.untag(cx) {
        ObjectType::Int(index) if index >= 0 => index as usize,
        _ => return Err(EvalError::signal(sym::WRONG_TYPE_ARGUMENT.into(), list![sym::WHOLENUMP,nframes.bind(cx);cx],cx).into()),
    };
    let snapshot = backtrace_snapshot(base.bind(cx),env,cx)?;
    root!(snapshot,cx);
    let selected = snapshot.bind(cx).as_list()?.nth(index).transpose()?;
    let Some(selected) = selected else { return Ok(NIL) };
    let [evaluated,function,args,flags] = crate::core::env::backtrace_components(selected)?;
    // No frame means no call: GNU does not validate a dormant callback.
    let callback_function: Function = callback.bind(cx).try_into().map_err(|_| {
            EvalError::signal(sym::INVALID_FUNCTION.into(), list![callback.bind(cx);cx], cx)
        })?;
    root!(callback_function,cx);
    call!(callback_function,evaluated,function,args,flags;env,cx).map_err(Into::into)
}

#[defun]
fn mapbacktrace<'ob>(callback: &Rto<Object>, base: Option<&Rto<Object>>, env: &mut Rt<Env>, cx: &'ob mut Context) -> Result<Object<'ob>> {
    let snapshot = backtrace_snapshot(base.map_or(NIL,|base|base.bind(cx)),env,cx)?;
    root!(snapshot,cx);
    rooted_iter!(frames,snapshot.bind(cx),cx);
    while let Some(entry) = frames.next()? {
        let [evaluated,function,args,flags] = crate::core::env::backtrace_components(entry.bind(cx))?;
        let callback_function: Function = callback.bind(cx).try_into().map_err(|_| {
            EvalError::signal(sym::INVALID_FUNCTION.into(), list![callback.bind(cx);cx], cx)
        })?;
        root!(callback_function,cx);
        call!(callback_function,evaluated,function,args,flags;env,cx)?;
    }
    Ok(NIL)
}

#[cfg(test)]
mod active_backtrace_tests {
    #[test]
    fn evaluated_frame_preserves_original_designator_and_values() {
        crate::interpreter::assert_lisp("(progn (fset 'bt-target '(lambda (x) (backtrace-frame--internal #'list 0 'bt-target))) (bt-target 17))", "(t bt-target (17) nil)");
    }
    #[test]
    fn alias_base_resolves_nearest_callable_identity() {
        crate::interpreter::assert_lisp("(progn (fset 'bt-target '(lambda (x) (backtrace-frame--internal #'list 0 'bt-alias))) (fset 'bt-alias 'bt-target) (bt-target 19))", "(t bt-target (19) nil)");
    }
    #[test]
    fn missing_frame_never_calls_callback() {
        crate::interpreter::assert_lisp("(progn (setq bt-seen nil) (list (backtrace-frame--internal (function (lambda (&rest x) (setq bt-seen t))) 999 nil) bt-seen))", "(nil nil)");
    }
    #[test]
    fn preargument_frame_retains_unevaluated_forms() {
        crate::interpreter::assert_lisp("(progn (fset 'bt-target '(lambda (x) x)) (bt-target (backtrace-frame--internal #'list 0 'bt-target)))", "(nil bt-target ((backtrace-frame--internal (function list) 0 (quote bt-target))) nil)");
    }
    #[test]
    fn callback_heap_return_survives_gc_and_reentrance() {
        crate::interpreter::assert_lisp("(progn (fset 'bt-target '(lambda (x) (backtrace-frame--internal (function (lambda (e f a flags) (garbage-collect) (backtrace-frame--internal #'list 999 nil) (list f a))) 0 'bt-target))) (bt-target '(1 2)))", "(bt-target ((1 2)))");
    }
    #[test]
    fn map_fixed_membership_ignores_callback_calls() {
        crate::interpreter::assert_lisp("(progn (setq bt-seen nil) (fset 'bt-target '(lambda () (mapbacktrace (function (lambda (e f a flags) (garbage-collect) (setq bt-seen (cons f bt-seen)))) 'bt-target))) (bt-target) (let ((tail bt-seen)) (while (cdr tail) (setq tail (cdr tail))) (eq (car tail) 'bt-target)))", "t");
    }
    #[test]
    fn callback_throw_restores_frames() {
        crate::interpreter::assert_lisp("(progn (fset 'bt-target '(lambda () (backtrace-frame--internal (function (lambda (&rest a) (throw 'bt-exit 7))) 0 'bt-target))) (list (catch 'bt-exit (bt-target)) (backtrace-frame--internal #'list 0 'bt-target)))", "(7 nil)");
    }
    #[test]
    fn interactive_marker_has_distinct_identity() {
        crate::interpreter::assert_lisp("(progn (fset 'bt-target '(lambda () (backtrace-frame--internal #'list 1 'bt-target))) (list (eq (symbol-function 'funcall) (symbol-function 'funcall-interactively)) (nth 1 (funcall-interactively 'bt-target))))", "(nil funcall-interactively)");
    }
}

#[cfg(test)]
mod active_backtrace_gnu_tests {
    #[test]
    fn native_contract_matches_gnu_batch() {
        for form in [
            "(progn (fset 'bt-gnu-target '(lambda (x) (backtrace-frame--internal #'list 0 'bt-gnu-target))) (bt-gnu-target 17))",
            "(progn (fset 'bt-gnu-target '(lambda (x) (backtrace-frame--internal #'list 0 'bt-gnu-alias))) (fset 'bt-gnu-alias 'bt-gnu-target) (bt-gnu-target 19))",
            "(progn (setq bt-gnu-seen nil) (list (backtrace-frame--internal (lambda (&rest x) (setq bt-gnu-seen t)) 999 nil) bt-gnu-seen))",
            "(progn (fset 'bt-gnu-target '(lambda () (backtrace-frame--internal #'list 1 'bt-gnu-target))) (list (eq (symbol-function 'funcall) (symbol-function 'funcall-interactively)) (nth 1 (funcall-interactively 'bt-gnu-target))))",
            "(progn (fset 'bt-gnu-target '(lambda () (backtrace-frame--internal (lambda (&rest a) (throw 'bt-gnu-exit 7)) 0 'bt-gnu-target))) (list (catch 'bt-gnu-exit (bt-gnu-target)) (backtrace-frame--internal #'list 0 'bt-gnu-target)))",
        ] { crate::assert_elprop!("{form}"); }
    }
}

#[cfg(test)]
mod active_backtrace_edge_tests {

    #[test]
    fn base_offset_narrows_to_c_int_before_positive_skip() {
        // This matrix targets the independently observed 32-bit C-int ABI.
        assert_eq!(std::mem::size_of::<std::os::raw::c_int>(), 4);
        for (offset, found) in [
            (-4_294_967_297_i64, true), (-4_294_967_296, true),
            (-2_147_483_649, false), (-2_147_483_648, true), (-1, true),
            (0, true), (1, false), (2_147_483_647, false),
            (2_147_483_648, true), (4_294_967_295, true),
            (4_294_967_296, true), (4_294_967_297, false),
        ] {
            let form = format!("(progn (fset 'bt-offset-target '(lambda (offset) (eq (nth 1 (backtrace-frame--internal #'list 0 (cons offset 'bt-offset-target))) 'bt-offset-target))) (bt-offset-target {offset}))");
            crate::interpreter::assert_lisp(&form, if found { "t" } else { "nil" });
            let form = format!("(progn (setq bt-offset-seen nil) (fset 'bt-offset-target '(lambda (offset) (mapbacktrace (function (lambda (e f a flags) (if (eq f 'bt-offset-target) (setq bt-offset-seen t)))) (cons offset 'bt-offset-target)))) (bt-offset-target {offset}) bt-offset-seen)");
            crate::interpreter::assert_lisp(&form, if found { "t" } else { "nil" });
        }
    }
    #[test]
    fn base_offset_c_int_boundaries_match_pinned_gnu() {
        assert_eq!(std::mem::size_of::<std::os::raw::c_int>(), 4);
        for offset in [2_147_483_647_i64, 2_147_483_648, 4_294_967_295, 4_294_967_296, 4_294_967_297, -2_147_483_648, -2_147_483_649] {
            let form = format!("(progn (fset 'bt-offset-gnu '(lambda (offset) (eq (nth 1 (backtrace-frame--internal #'list 0 (cons offset 'bt-offset-gnu))) 'bt-offset-gnu))) (bt-offset-gnu {offset}))");
            crate::assert_elprop!("{form}");
        }
    }
    #[test]
    fn selected_noncallable_callback_signals_invalid_function_payload() {
        for callback in ["17", "[1 2]", "\"not a function\""] {
            let form = format!("(condition-case e (backtrace-frame--internal {callback} 0 nil) (invalid-function (list (car e) (equal (car (cdr e)) {callback}))))");
            crate::interpreter::assert_lisp(&form, "(invalid-function t)");
            let form = format!("(condition-case e (mapbacktrace {callback} nil) (invalid-function (list (car e) (equal (car (cdr e)) {callback}))))");
            crate::interpreter::assert_lisp(&form, "(invalid-function t)");
        }
    }
    #[test]
    fn selected_noncallable_condition_matches_gnu() {
        for form in [
            "(condition-case e (backtrace-frame--internal 17 0 nil) (invalid-function e))",
            "(condition-case e (mapbacktrace [1 2] nil) (invalid-function e))",
            "(backtrace-frame--internal 17 999 nil)",
            "(condition-case e (backtrace-frame--internal 17 -1 nil) (wrong-type-argument e))",
        ] { crate::assert_elprop!("{form}"); }
    }

    #[test]
    fn map_callback_signal_unwinds_then_next_call_succeeds() {
        crate::interpreter::assert_lisp("(progn (fset 'mb-target '(lambda (callback) (mapbacktrace callback 'mb-target))) (list (condition-case nil (mb-target (function (lambda (&rest args) (garbage-collect) (backtrace-frame--internal #'list 999 nil) (error \"callback failure\")))) (error 'caught)) (backtrace-frame--internal #'list 0 'mb-target) (mb-target (function (lambda (&rest args) nil)))))", "(caught nil nil)");
    }
    #[test]
    fn map_callback_reentry_keeps_snapshot_membership() {
        crate::interpreter::assert_lisp("(progn (setq mb-seen nil mb-visits 0) (fset 'mb-reentry '(lambda () (garbage-collect) (backtrace-frame--internal #'list 999 nil))) (fset 'mb-target '(lambda () (mapbacktrace (function (lambda (e f a flags) (mb-reentry) (setq mb-seen (cons f mb-seen)) (if (eq f 'mb-target) (setq mb-visits (1+ mb-visits))))) 'mb-target))) (list (mb-target) mb-visits (memq 'mb-reentry mb-seen) (backtrace-frame--internal #'list 0 'mb-target)))", "(nil 1 nil nil)");
    }
    #[test]
    fn missing_frame_does_not_validate_dormant_callback() {
        for form in [
            "(backtrace-frame--internal 17 999 nil)",
            "(backtrace-frame--internal 17 0 '(0 . nil))",
            "(mapbacktrace 17 'bt-no-such-base)",
            "(mapbacktrace 17 '(0 . nil))",
        ] { crate::interpreter::assert_lisp(form, "nil"); }
        crate::interpreter::assert_lisp("(condition-case e (backtrace-frame--internal 17 -1 nil) (wrong-type-argument e))", "(wrong-type-argument wholenump -1)");
        crate::interpreter::assert_lisp("(condition-case nil (backtrace-frame--internal 17 0 nil) (error 'caught))", "caught");
        crate::interpreter::assert_lisp("(condition-case nil (mapbacktrace 17 nil) (error 'caught))", "caught");
    }
    #[test]
    fn dormant_callback_and_index_order_match_gnu() {
        for form in [
            "(backtrace-frame--internal 17 999 nil)",
            "(backtrace-frame--internal 17 0 '(0 . nil))",
            "(mapbacktrace 17 'bt-no-such-base)",
            "(mapbacktrace 17 '(0 . nil))",
            "(condition-case e (backtrace-frame--internal 17 -1 nil) (wrong-type-argument e))",
            "(condition-case nil (backtrace-frame--internal 17 0 nil) (error 'caught))",
            "(condition-case nil (mapbacktrace 17 nil) (error 'caught))",
        ] { crate::assert_elprop!("{form}"); }
    }
    #[test]
    fn cons_nil_base_does_not_mean_unrestricted_stack() {
        for base in ["'(0 . nil)", "'(1 . nil)", "'(-1 . nil)"] {
            let inspect = format!("(backtrace-frame--internal #'list 0 {base})");
            crate::interpreter::assert_lisp(&inspect, "nil");
            let map = format!("(let ((seen nil)) (mapbacktrace (function (lambda (&rest args) (setq seen t))) {base}) seen)");
            crate::interpreter::assert_lisp(&map, "nil");
        }
        crate::interpreter::assert_lisp("(null (null (backtrace-frame--internal #'list 0 nil)))", "t");
        crate::interpreter::assert_lisp("(let ((seen nil)) (mapbacktrace (function (lambda (&rest args) (setq seen t))) nil) seen)", "t");
    }
    #[test]
    fn absent_base_does_not_match_unbound_special_function_cells() {
        crate::interpreter::assert_lisp("(let ((x 1)) (backtrace-frame--internal #'list 0 'bt-never-defined))", "nil");
    }
    #[test]
    fn special_frame_uses_original_forms_and_alias_base() {
        // Compare the whole frame: (car nil) is nil too, so checking only
        // the evaluated flag cannot distinguish a missing frame.
        crate::interpreter::assert_lisp("(progn (fset 'bt-let-alias 'let) (let ((x 1)) (backtrace-frame--internal #'list 0 'bt-let-alias)))", "(nil let (((x 1)) (backtrace-frame--internal (function list) 0 (quote bt-let-alias))) nil)");
    }
    #[test]
    fn invalid_index_reports_gnu_wholenump_condition() {
        crate::interpreter::assert_lisp("(condition-case e (backtrace-frame--internal #'list -1 nil) (wrong-type-argument e))", "(wrong-type-argument wholenump -1)");
    }
}

#[cfg(test)]
mod run_hooks_shared_tests {
    #[test]
    fn zero_nil_unbound_raw_lambda_and_closure() {
        crate::interpreter::assert_lisp(r#"(progn (defvar rh-seen) (defvar rh-empty) (defvar rh-lambda) (defvar rh-closure) (let ((rh-seen nil) (rh-empty nil) (rh-lambda '(lambda () (setq rh-seen (cons 'lambda rh-seen)))) (rh-closure '(closure ((captured . 8)) () (garbage-collect) (setq rh-seen (cons captured rh-seen))))) (list (run-hooks) (run-hooks 'rh-unbound 'rh-empty 'rh-lambda 'rh-closure) rh-seen)))"#, "(nil nil (8 lambda))");
    }
    #[test]
    fn multiple_hooks_and_nonnil_results_keep_source_order() {
        crate::interpreter::assert_lisp(r#"(progn (defvar rh-seen) (defvar rh-a) (defvar rh-b) (let ((rh-seen nil) (rh-a (list (function (lambda () (setq rh-seen (cons 'a rh-seen)) t)) (function (lambda () (garbage-collect) (setq rh-seen (cons 'b rh-seen))))) ) (rh-b (function (lambda () (setq rh-seen (cons 'c rh-seen)))))) (list (run-hooks 'rh-a 'rh-b) rh-seen)))"#, "(nil (c b a))");
    }
    #[test]
    fn default_t_marker_ignores_recursive_default_t() {
        crate::interpreter::assert_lisp(r#"(progn (defvar rh-count) (defvar rh-hooks) (let ((rh-count 0) (rh-hooks nil)) (setq rh-hooks (list t (function (lambda () (garbage-collect) (setq rh-count (1+ rh-count)))))) (list (run-hooks 'rh-hooks) rh-count)))"#, "(nil 2)");
    }
    #[test]
    fn callback_mutates_live_tail_across_gc() {
        crate::interpreter::assert_lisp(r#"(progn (defvar rh-seen) (defvar rh-hooks) (let ((rh-seen nil) (rh-hooks nil)) (setq rh-hooks (list (function (lambda () (setq rh-seen (cons 'first rh-seen)) (setcdr rh-hooks (list (function (lambda () (setq rh-seen (cons 'replacement rh-seen)))))) (garbage-collect) t)) (function (lambda () (error "replaced"))))) (list (run-hooks 'rh-hooks) rh-seen)))"#, "(nil (replacement first))");
    }
    #[test]
    fn error_and_throw_stop_later_hooks_and_next_call_recovers() {
        crate::interpreter::assert_lisp(r#"(progn (defvar rh-seen) (defvar rh-bad) (defvar rh-throw) (defvar rh-later) (let ((rh-seen nil) (rh-bad (function (lambda () (garbage-collect) (error "expected")))) (rh-throw (function (lambda () (throw 'rh-exit 'thrown)))) (rh-later (function (lambda () (setq rh-seen 'later))))) (let ((caught (condition-case nil (run-hooks 'rh-bad 'rh-later) (error t))) (thrown (catch 'rh-exit (run-hooks 'rh-throw 'rh-later)))) (list caught thrown rh-seen (run-hooks 'rh-later) rh-seen))))"#, "(t thrown nil nil later)");
    }
    #[test]
    fn real_cycle_reaches_existing_budget_and_recovers() {
        crate::interpreter::assert_lisp(r#"(progn (defvar rh-count) (defvar rh-hooks) (defvar rh-stop) (let ((rh-count 0) (rh-hooks nil) (rh-stop nil)) (setq rh-hooks (list (function (lambda () (setq rh-count (1+ rh-count)))))) (setcdr rh-hooks rh-hooks) (setq rh-stop (condition-case nil (run-hooks 'rh-hooks) (error 'bounded))) (setcdr rh-hooks nil) (list rh-stop rh-count (run-hooks 'rh-hooks) rh-count)))"#, "(bounded 65536 nil 65537)");
    }
    #[test]
    fn ordinary_finite_cases_match_gnu() {
        for form in [
            "(let ((rh-hooks '(lambda () 'ignored))) (run-hooks 'rh-hooks))",
            "(let ((rh-count 0) (rh-hooks nil)) (setq rh-hooks (list t (lambda () (setq rh-count (1+ rh-count))))) (run-hooks 'rh-hooks) rh-count)",
            r#"(let ((rh-hooks (eval '(let ((captured 8)) (lambda () (if (= captured 8) t (error "capture lost")))) t))) (run-hooks 'rh-hooks))"#,
        ] { crate::assert_elprop!("{form}"); }
    }
}

#[cfg(test)]
mod hook_wrapped_tests {
    #[test]
    fn empty_hook_does_not_validate_wrapper_and_raw_entries_are_data() {
        crate::interpreter::assert_lisp(r#"(progn (defvar hw-empty) (defvar hw-entry) (let ((hw-empty nil) (hw-entry 42)) (list (run-hook-wrapped 'hw-missing 'undefined-wrapper) (run-hook-wrapped 'hw-empty 99) (run-hook-wrapped 'hw-entry (function (lambda (fun x) (list fun x))) 'payload))))"#, "(nil nil (42 payload))");
    }
    #[test]
    fn wrapper_receives_original_entry_and_heap_args_then_short_circuits() {
        crate::interpreter::assert_lisp(r#"(progn (defvar hw-seen) (defvar hw-payload) (defvar hw-hooks) (let ((hw-seen nil) (hw-payload (list 'payload)) (hw-hooks '(first second third))) (let ((answer (run-hook-wrapped 'hw-hooks (function (lambda (fun arg) (garbage-collect) (setq hw-seen (cons fun hw-seen)) (if (eq fun 'second) arg nil))) hw-payload))) (list (eq answer hw-payload) hw-seen))))"#, "(t (second first))");
    }
    #[test]
    fn real_wrapper_invokes_lambda_with_zero_original_arguments() {
        crate::interpreter::assert_lisp(r#"(progn (defvar hw-hook) (let ((hw-hook '(lambda () 'called))) (run-hook-wrapped 'hw-hook (function (lambda (fun) (garbage-collect) (funcall fun))))))"#, "called");
    }
    #[test]
    fn default_marker_uses_same_wrapper_and_live_tail_reloads() {
        crate::interpreter::assert_lisp(r#"(progn (defvar hw-hooks) (defvar hw-seen) (let ((hw-hooks '(t first)) (hw-seen nil)) (run-hook-wrapped 'hw-hooks (function (lambda (fun) (setq hw-seen (cons fun hw-seen)) nil))) hw-seen))"#, "(first first)");
        crate::interpreter::assert_lisp(r#"(progn (defvar hw-hooks) (defvar hw-seen) (let ((hw-hooks (list 'first 'replaced)) (hw-seen nil)) (run-hook-wrapped 'hw-hooks (function (lambda (fun) (setq hw-seen (cons fun hw-seen)) (if (eq fun 'first) (setcdr hw-hooks (list 'last))) (garbage-collect) nil))) hw-seen))"#, "(last first)");
    }
    #[test]
    fn nested_wrappers_keep_original_args_and_throw_error_unwind() {
        crate::interpreter::assert_lisp(r#"(progn (defvar hw-hook) (defvar hw-nested) (defvar hw-payload) (let ((hw-hook 'entry) (hw-nested nil) (hw-payload (list 'outer))) (let ((answer (run-hook-wrapped 'hw-hook (function (lambda (fun arg) (setq hw-nested (run-hook-wrapped 'hw-hook (function (lambda (f x) (garbage-collect) (list f x))) 'inner)) (garbage-collect) arg)) hw-payload))) (list (eq answer hw-payload) hw-nested (catch 'hw-exit (run-hook-wrapped 'hw-hook (function (lambda (f) (throw 'hw-exit 'thrown))))) (condition-case nil (run-hook-wrapped 'hw-hook (function (lambda (f) (error "expected")))) (error 'caught)) (run-hook-wrapped 'hw-hook (function (lambda (f) 'recovered)))))))"#, "(t (entry inner) thrown caught recovered)");
    }
    #[test]
    fn cycle_is_bounded_without_calling_entries() {
        crate::interpreter::assert_lisp(r#"(progn (defvar hw-hook) (defvar hw-count) (defvar hw-status) (let ((hw-hook (list 42)) (hw-count 0) (hw-status nil)) (setcdr hw-hook hw-hook) (setq hw-status (condition-case nil (run-hook-wrapped 'hw-hook (function (lambda (f) (setq hw-count (1+ hw-count)) nil))) (error 'bounded))) (setcdr hw-hook nil) (list hw-status hw-count (run-hook-wrapped 'hw-hook (function (lambda (f) f))))))"#, "(bounded 65536 42)");
    }
    #[test]
    fn finite_wrapped_contract_matches_gnu() {
        for form in [
            "(let ((hw-hook nil)) (run-hook-wrapped 'hw-hook 99))",
            "(let ((hw-hook 42)) (run-hook-wrapped 'hw-hook (lambda (f x) (list f x)) 'payload))",
            "(let ((hw-hook '(first second third))) (run-hook-wrapped 'hw-hook (lambda (f) (if (eq f 'second) f nil))))",
            "(let ((hw-hook (eval '(let ((captured 8)) (lambda () captured)) t))) (run-hook-wrapped 'hw-hook (lambda (f) (funcall f))))",
        ] { crate::assert_elprop!("{form}"); }
    }
}
