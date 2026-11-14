//! The basic elisp interpreter.
use crate::{
    core::{
        cons::{Cons, ElemStreamIter, IntoArray},
        env::{CallFrame, Env, sym},
        error::{Type, TypeError},
        gc::{Context, Rt, Rto, Slot},
        object::{Function, Gc, List, ListType, NIL, Object, ObjectType, Symbol, TRUE, TagType},
    },
    data::LispError,
    eval::{ErrorType, EvalError, EvalResult, add_trace},
    rooted_iter,
};
use anyhow::Context as _;
use anyhow::Result as AnyResult;
use anyhow::{bail, ensure};
use fallible_streaming_iterator::FallibleStreamingIterator;
use rune_core::macros::{bail_err, call, error, rebind, root};
use rune_macros::defun;

struct Interpreter<'brw, 'rt> {
    vars: &'brw mut Rt<Vec<Slot<&'rt Cons>>>,
    env: &'brw mut Rt<Env<'rt>>,
}

#[defun]
pub(crate) fn eval<'ob>(
    form: &Rto<Object>,
    lexical: Option<&Rto<Object>>,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>, anyhow::Error> {
    cx.garbage_collect(false);
    root!(vars, new(Vec<Slot<&Cons>>), cx);
    if let Some(ObjectType::Cons(cons)) = lexical.map(|x| x.untag(cx)) {
        for var in cons.elements() {
            if let Some(binding) = var?.cons() {
                vars.push(binding);
            }
        }
    }
    let mut interpreter = Interpreter { vars, env };
    interpreter.eval_form(form, cx).map_err(Into::into)
}

impl Interpreter<'_, '_> {
    fn eval_form<'ob>(&mut self, rt: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        match rt.untag(cx) {
            ObjectType::Symbol(sym) => self.var_ref(sym, cx),
            ObjectType::Cons(_) => {
                let x = rt.try_as().unwrap();
                self.eval_sexp(x, cx)
            }
            _ => Ok(rt.bind(cx)),
        }
    }

    pub(crate) fn eval_sexp<'ob>(
        &mut self,
        cons: &Rto<Gc<&Cons>>,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        let cons = cons.bind(cx);
        let forms = cons.cdr();
        root!(forms, cx);
        match cons.car().untag() {
            ObjectType::Symbol(sym) => match sym {
                sym::QUOTE => self.quote(forms.bind(cx), cx),
                sym::LET => self.eval_let(forms, true, cx),
                sym::LET_STAR => self.eval_let(forms, false, cx),
                sym::IF => self.eval_if(forms, cx),
                sym::AND => self.eval_and(forms, cx),
                sym::OR => self.eval_or(forms, cx),
                sym::COND => self.eval_cond(forms, cx),
                sym::WHILE => self.eval_while(forms, cx),
                sym::PROGN | sym::INLINE => self.eval_progn(forms, cx),
                sym::PROG1 => self.eval_progx(forms, 1, cx),
                sym::PROG2 => self.eval_progx(forms, 2, cx),
                sym::SETQ => self.setq(forms, cx),
                sym::DEFVAR => self.defvar(forms, cx),
                sym::DEFCONST => self.defconst(forms, cx),
                sym::FUNCTION => self.eval_function(forms, cx),
                sym::INTERACTIVE => Ok(NIL), // TODO: implement
                sym::CATCH => self.catch(forms, cx),
                sym::CONDITION_CASE => self.condition_case(forms, cx),
                sym::SAVE_CURRENT_BUFFER => self.save_current_buffer(forms, cx),
                sym::SAVE_EXCURSION => self.save_excursion(forms, cx),
                sym::UNWIND_PROTECT => self.unwind_protect(forms, cx),
                _ => {
                    root!(sym, cx);
                    self.eval_call(sym, forms, cx)
                }
            },
            other => Err(error!("Invalid Function: {other}")),
        }
    }

    fn catch<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        let Some(tag) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::CATCH, 1, 0, cx))
        };
        // Evaluate before installing the tag, so errors cannot leak a catch.
        let tag = rebind!(self.eval_form(tag, cx)?);
        self.env.catch_stack.push(tag);
        let result = match self.implicit_progn(forms, cx) {
            Ok(x) => Ok(rebind!(x, cx)),
            Err(e) => {
                if let ErrorType::Throw(payload) = &e.error {
                    match payload.bind(cx) {
                        Ok(pair) => {
                            let catch_tag = self.env.catch_stack.last().unwrap();
                            if catch_tag.bind(cx).ptr_eq(pair.car()) {
                                Ok(pair.cdr())
                            } else {
                                Err(e)
                            }
                        }
                        Err(error) => Err(error.into()),
                    }
                } else {
                    Err(e)
                }
            }
        };
        // pop this tag from the catch stack
        self.env.catch_stack.pop();
        result
    }

    fn defconst<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        let Some(name) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::DEFCONST, 2, 0, cx))
        };
        let name: Symbol = name.bind(cx).try_into()?;
        root!(name, cx);
        let Some(initializer) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::DEFCONST, 2, 1, cx))
        };
        root!(initializer, cx);
        // Documentation is unevaluated. Validate the complete argument shape
        // before running the initializer, including an improper trailing list.
        forms.next()?;
        if forms.next()?.is_some() {
            bail_err!("Too many arguments");
        }
        let value = rebind!(self.eval_form(initializer, cx)?);
        let name = name.bind(cx);
        self.env.set_var(name, value)?;
        name.make_special();
        // DEFCONST declares special storage, not an immutable value/function
        // cell: later SETQ, rebinding and redefinition remain permitted.
        Ok(name.into())
    }

    fn defvar<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        // (defvar x ...)                 // (defvar)
        let Some(sym) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::DEFVAR, 1, 0, cx))
        };
        let name: Symbol = sym.bind(cx).try_into()?;
        root!(name, cx);
        let value = match forms.next()? {
            // (defvar x y)
            Some(value) => rebind!(self.eval_form(value, cx)?),
            // (defvar x)
            None => NIL,
        };
        self.env.defvar(name.bind(cx), value)?;
        Ok(value)
    }

    fn eval_call<'ob>(
        &mut self,
        sym: &Rto<Symbol>,
        args: &Rto<Object>,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        let Some(func) = sym.bind(cx).follow_indirect(cx) else {
            bail_err!("Invalid function: {sym}")
        };
        root!(func, cx);

        match func.bind(cx).as_cons_pair() {
            Ok((sym::AUTOLOAD, _)) => {
                crate::eval::autoload_do_load(func.cast(), None, None, self.env, cx)
                    .map_err(|e| add_trace(e, "autoload", &[]))?;
                func.set(sym.bind(cx).follow_indirect(cx).unwrap());
            }
            Ok((sym::MACRO, mcro)) => {
                let iter = args.bind(cx).into_list()?;
                let mut frame = CallFrame::new(self.env);
                for arg in iter {
                    frame.push_arg(arg?);
                }
                root!(mcro, mcro.tag(), cx);
                let name = sym.bind(cx).name().to_owned();
                let value = mcro.call(&mut frame, Some(&name), cx)?;
                drop(frame);
                root!(value, cx);
                return self.eval_form(value, cx);
            }
            _ => {}
        }

        // Native functions validate arity (and proper argument-list shape)
        // before evaluating any argument, including throw's tag/value forms.
        if let crate::core::object::FunctionType::SubrFn(subr) = func.bind(cx).untag() {
            let mut count = 0usize;
            for argument in args.bind(cx).into_list()? {
                argument?;
                count += 1;
            }
            let required = usize::from(subr.args.required);
            let maximum = required + usize::from(subr.args.optional);
            if count < required || (!subr.args.rest && count > maximum) {
                let expected = if count < required { required } else { maximum };
                let expected =
                    u16::try_from(expected).context("Native arity exceeds supported range")?;
                let count = u16::try_from(count).context("Too many native arguments")?;
                bail_err!(LispError::arg_cnt(sym.bind(cx), expected, count, cx));
            }
        }

        rooted_iter!(iter, args, cx);
        root!(args, new(Vec), cx);
        while let Some(x) = iter.next()? {
            let result = self.eval_form(x, cx)?;
            args.push(result);
        }
        let frame = &mut CallFrame::new(self.env);
        frame.push_arg_slice(Rt::bind_slice(args, cx));
        let name = sym.bind(cx).name().to_owned();
        func.call(frame, Some(&name), cx)
    }

    fn eval_function<'ob>(
        &mut self,
        obj: &Rto<Object<'ob>>,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        let form = match obj.bind(cx).into_array()? {
            Ok([x]) => x,
            Err(e) => bail_err!(LispError::arg_cnt(sym::FUNCTION, 1, e, cx)),
        };
        root!(form, cx); // Polonius
        let Ok((sym::LAMBDA, doc)) = form.bind(cx).as_cons_pair() else {
            return Ok(form.bind(cx));
        };
        root!(doc, doc.tag(), cx);
        let body = rebind!(self.replace_doc_symbol(doc, cx)?);
        let env = {
            let vars = self.vars.bind_ref(cx);
            let mut tail = Object::from(Cons::new1(true, cx));
            for var in vars {
                tail = Cons::new(**var, tail, cx).into();
            }
            tail
        };
        // This function will trim the environment down to only what is actually
        // captured in the closure
        if let Some(closure_fn) = self.env.vars.get(sym::INTERNAL_MAKE_INTERPRETED_CLOSURE_FUNCTION)
            && closure_fn.bind(cx) != sym::NIL
        {
            let closure_fn: Result<&Rto<Function>, _> = closure_fn.try_as();
            if let Ok(closure_fn) = closure_fn {
                let lambda = Object::from(Cons::new(sym::LAMBDA, body, cx));
                let closure_fn: Function = closure_fn.bind(cx);
                root!(closure_fn, cx);
                return call!(closure_fn, lambda, env; self.env, cx);
            }
        }
        // If the closure capture function is not defined, use the whole environment
        let end = Cons::new(env, body, cx);
        Ok(Cons::new(sym::CLOSURE, end, cx).into())
    }

    /// Handle special case of (:documentation form) to build the docstring
    /// dynamically. If the docstring is not of this form, just return the current body.
    fn replace_doc_symbol<'ob>(
        &mut self,
        quoted: &Rto<Object>,
        cx: &'ob mut Context,
    ) -> Result<Object<'ob>, EvalError> {
        let Some((arg_list, doc, body)) = (|| {
            let mut forms = quoted.bind(cx).as_list().ok()?;
            let arg_list = forms.next()?.ok()?;
            let doc = forms.next()?.ok()?;
            let body = forms.rest().ok()?.map(Into::into).unwrap_or(NIL);
            match doc.as_cons_pair() {
                Ok((sym::KW_DOCUMENTATION, ObjectType::Cons(doc))) => {
                    Some((arg_list, doc, cx.bind(body)))
                }
                _ => None,
            }
        })() else {
            return Ok(quoted.bind(cx));
        };
        root!(arg_list, cx);
        root!(body, cx);
        // ((<args..>) (doc_str) ...)
        let docstring = {
            root!(doc_form, doc.car(), cx);
            let docstring = rebind!(self.eval_form(doc_form, cx)?);
            // Handle the special case of oclosure docstrings being a symbol
            match docstring.untag() {
                ObjectType::Symbol(sym) => cx.add(sym.name()),
                _ => docstring,
            }
        };
        // ((<args..>) (:documentation <form>) body)
        Ok(Cons::new(arg_list, Cons::new(docstring, body, cx), cx).into())
    }

    fn eval_progx<'ob>(
        &mut self,
        obj: &Rto<Object>,
        prog_num: u16,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        let mut count = 0;
        root!(returned_form, None::<Object>, cx);
        rooted_iter!(forms, obj, cx);
        while let Some(form) = forms.next()? {
            let value = self.eval_form(form, cx)?;
            count += 1;
            if prog_num == count {
                returned_form.set(Some(value));
            }
        }
        match &**returned_form {
            Some(x) => Ok(x.bind(cx)),
            None => {
                let name = match prog_num {
                    1 => sym::PROG1,
                    2 => sym::PROG2,
                    _ => sym::PROGN,
                };
                Err(LispError::arg_cnt(name, prog_num, count, cx).into())
            }
        }
    }

    fn eval_progn<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        self.implicit_progn(forms, cx)
    }

    fn eval_while<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        let (condition, body) = {
            let list: List = obj.bind(cx).try_into()?;
            match list.untag() {
                ListType::Nil => bail_err!(LispError::arg_cnt(sym::WHILE, 1, 0, cx)),
                ListType::Cons(cons) => (cons.car(), cons.cdr()),
            }
        };
        root!(condition, cx);
        root!(body, cx);
        while self.eval_form(condition, cx)? != NIL {
            rooted_iter!(forms, &*body, cx);
            self.implicit_progn(forms, cx)?;
        }
        Ok(NIL)
    }

    fn eval_cond<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        while let Some(form) = forms.next()? {
            rooted_iter!(clause, form, cx);
            if let Some(first) = clause.next()? {
                let condition = self.eval_form(first, cx)?;
                if condition != NIL {
                    return if clause.is_empty() {
                        Ok(rebind!(condition, cx))
                    } else {
                        self.implicit_progn(clause, cx)
                    };
                }
            }
        }
        Ok(NIL)
    }

    fn eval_and<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        root!(last, TRUE, cx);
        rooted_iter!(forms, obj, cx);
        while let Some(form) = forms.next()? {
            let result = self.eval_form(form, cx)?;
            if result == NIL {
                return Ok(NIL);
            }
            last.set(result);
        }
        Ok(last.bind(cx))
    }

    fn eval_or<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        while let Some(form) = forms.next()? {
            let result = self.eval_form(form, cx)?;
            if result != NIL {
                return Ok(rebind!(result, cx));
            }
        }
        Ok(NIL)
    }

    fn eval_if<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        let Some(condition) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::IF, 2, 0, cx))
        };
        root!(condition, cx);
        let Some(true_branch) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::IF, 2, 1, cx))
        };
        root!(true_branch, cx);
        #[expect(clippy::if_not_else)]
        if self.eval_form(condition, cx)? != NIL {
            self.eval_form(true_branch, cx)
        } else {
            self.implicit_progn(forms, cx)
        }
    }

    fn setq<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        let mut arg_cnt = 0;
        root!(last_value, NIL, cx);
        while let Some((var, val)) = Self::pairs(&mut forms, cx)? {
            match (var.untag(), val) {
                (ObjectType::Symbol(var), Some(val)) => {
                    root!(var, cx);
                    root!(val, cx);
                    let val = rebind!(self.eval_form(val, cx)?);
                    self.var_set(var.bind(cx), val, cx)?;
                    last_value.set(val);
                }
                (_, Some(_)) => bail_err!(TypeError::new(Type::Symbol, var)),
                (_, None) => bail_err!(LispError::arg_cnt(sym::SETQ, arg_cnt, arg_cnt + 1, cx)),
            }
            arg_cnt += 2;
        }
        if arg_cnt < 2 {
            Err(LispError::arg_cnt(sym::SETQ, 2, arg_cnt, cx).into())
        } else {
            Ok(last_value.bind(cx))
        }
    }

    fn pairs<'ob>(
        iter: &mut ElemStreamIter<'_>,
        cx: &'ob Context,
    ) -> AnyResult<Option<(Object<'ob>, Option<Object<'ob>>)>> {
        let first = iter.next()?.map(|x| x.bind(cx));
        let second = iter.next()?.map(|x| x.bind(cx));
        Ok(first.map(|first| (first, second)))
    }

    fn var_ref<'ob>(&self, sym: Symbol, cx: &'ob Context) -> EvalResult<'ob> {
        if sym.is_const() {
            Ok(sym.into())
        } else {
            let mut iter = self.vars.iter().rev();
            match iter.find_map(|cons| (cons.car(cx) == sym).then(|| cons.cdr(cx))) {
                Some(value) => Ok(value),
                None => match self.env.vars.get(sym) {
                    Some(v) => Ok(v.bind(cx)),
                    None => Err(error!("Void variable: {sym}")),
                },
            }
        }
    }

    fn var_set(&mut self, name: Symbol, new_value: Object, cx: &Context) -> AnyResult<()> {
        let mut iter = self.vars.iter().rev();
        match iter.find(|cons| cons.car(cx) == name) {
            Some(value) => {
                value.bind(cx).set_cdr(new_value).expect("variables should never be immutable");
                Ok(())
            }
            None => self.env.set_var(name, new_value),
        }
    }

    fn quote<'ob>(&self, value: Object<'ob>, cx: &Context) -> EvalResult<'ob> {
        match value.into_array()? {
            Ok([x]) => Ok(x),
            Err(e) => Err(LispError::arg_cnt(sym::QUOTE, 1, e, cx).into()),
        }
    }

    fn eval_let<'ob>(
        &mut self,
        form: &Rto<Object>,
        parallel: bool,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        rooted_iter!(iter, form, cx);
        let prev_len = self.vars.len();
        // (let x ...)                   // (let)
        let Some(obj) = iter.next()? else { bail_err!(LispError::arg_cnt(sym::LET, 1, 0, cx)) };
        let mut varbind_count = 0;
        let bindings = if parallel {
            self.let_bind_parallel(obj, &mut varbind_count, cx)
        } else {
            self.let_bind_serial(obj, &mut varbind_count, cx)
        };
        let result = match bindings {
            Ok(()) => match self.implicit_progn(iter, cx) {
                Ok(object) => Ok(rebind!(object, cx)),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        // Restore both lexical and dynamic bindings on every exit, including
        // failed let* initializers which have already installed some bindings.
        self.vars.truncate(prev_len);
        self.env.unbind(varbind_count, cx);
        result
    }

    fn let_bind_serial(
        &mut self,
        form: &Rto<Object>,
        varbind_count: &mut u16,
        cx: &mut Context,
    ) -> Result<(), EvalError> {
        rooted_iter!(bindings, form, cx);
        while let Some(binding) = bindings.next()? {
            match binding.untag(cx) {
                // (let ((x y)))
                ObjectType::Cons(_) => {
                    let cons = binding.as_cons();
                    let val = rebind!(self.let_bind_value(cons, cx)?);
                    let var: Symbol =
                        cons.untag(cx).car().try_into().context("let variable must be a symbol")?;
                    self.create_let_binding(var, val, varbind_count, cx)?;
                }
                // (let (x))
                ObjectType::Symbol(sym) => {
                    self.create_let_binding(sym, NIL, varbind_count, cx)?;
                }
                // (let (1))
                x => bail_err!(TypeError::new(Type::Cons, x)),
            }
        }
        Ok(())
    }

    fn let_bind_parallel(
        &mut self,
        form: &Rto<Object>,
        varbind_count: &mut u16,
        cx: &mut Context,
    ) -> Result<(), EvalError> {
        root!(let_bindings, new(Vec<(Slot<Symbol>, Slot<Object>)>), cx);
        rooted_iter!(bindings, form, cx);
        while let Some(binding) = bindings.next()? {
            match binding.untag(cx) {
                // (let ((x y)))
                ObjectType::Cons(_) => {
                    let cons = binding.as_cons();
                    let var = rebind!(self.let_bind_value(cons, cx)?);
                    let sym: Symbol =
                        cons.untag(cx).car().try_into().context("let variable must be a symbol")?;
                    let_bindings.push((sym, var));
                }
                // (let (x))
                ObjectType::Symbol(sym) => {
                    let_bindings.push((sym, NIL));
                }
                // (let (1))
                x => bail_err!(TypeError::new(Type::Cons, x)),
            }
        }
        for (var, val) in let_bindings.bind_ref(cx) {
            self.create_let_binding(**var, **val, varbind_count, cx)?;
        }
        Ok(())
    }

    fn create_let_binding(
        &mut self,
        var: Symbol,
        val: Object,
        varbind_count: &mut u16,
        cx: &Context,
    ) -> Result<(), EvalError> {
        if var.is_special() {
            let next = varbind_count
                .checked_add(1)
                .ok_or_else(|| error!("Too many dynamic let bindings"))?;
            self.env.varbind(var, val, cx)?;
            *varbind_count = next;
        } else {
            self.vars.push(Cons::new(var, val, cx));
        }
        Ok(())
    }

    fn let_bind_value<'ob>(
        &mut self,
        cons: &Rto<Gc<&Cons>>,
        cx: &'ob mut Context,
    ) -> Result<Object<'ob>, EvalError> {
        rooted_iter!(iter, cons.bind(cx).cdr(), cx);
        let value = match iter.next()? {
            // (let ((x y)))
            Some(x) => self.eval_form(x, cx)?,
            // (let ((x)))
            None => NIL,
        };
        // (let ((x y z ..)))
        if !iter.is_empty() {
            bail_err!("Let binding can only have 1 value");
        }
        Ok(value)
    }

    fn implicit_progn<'ob>(
        &mut self,
        mut forms: ElemStreamIter<'_>,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        root!(last, NIL, cx);
        while let Some(form) = forms.next()? {
            let value = self.eval_form(form, cx)?;
            last.set(value);
        }
        Ok(last.bind(cx))
    }

    fn unwind_protect<'ob>(&mut self, obj: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, obj, cx);
        let Some(body) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::UNWIND_PROTECT, 1, 0, cx))
        };
        match self.eval_form(body, cx) {
            Ok(x) => {
                root!(x, cx);
                self.implicit_progn(forms, cx)?;
                Ok(x.bind(cx))
            }
            Err(e) => {
                self.implicit_progn(forms, cx)?;
                Err(e)
            }
        }
    }

    fn save_excursion<'ob>(&mut self, form: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        // The token contains owner-local native handle/marker IDs, not a heap
        // reference or a long-lived editor borrow across arbitrary Lisp code.
        let token = self.env.save_excursion(cx)?;
        let result = rebind!(self.eval_progn(form, cx));
        self.env.restore_excursion(token, cx);
        result
    }

    fn save_current_buffer<'ob>(
        &mut self,
        form: &Rto<Object>,
        cx: &'ob mut Context,
    ) -> EvalResult<'ob> {
        let buffer = self.env.current_buffer_handle();
        let result = rebind!(self.eval_progn(form, cx));
        // Killing the saved buffer is allowed. Do not replace a pending body
        // error/throw with a failed attempt to restore that expired handle.
        let _ = self.env.set_current_buffer(buffer, cx);
        result
    }

    fn condition_case<'ob>(&mut self, form: &Rto<Object>, cx: &'ob mut Context) -> EvalResult<'ob> {
        rooted_iter!(forms, form, cx);
        let Some(var) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::CONDITION_CASE, 2, 0, cx))
        };
        root!(var, cx);
        let Some(bodyform) = forms.next()? else {
            bail_err!(LispError::arg_cnt(sym::CONDITION_CASE, 2, 1, cx))
        };
        let err = match self.eval_form(bodyform, cx) {
            Ok(x) => return Ok(rebind!(x, cx)),
            Err(e) => e,
        };
        if matches!(err.error, ErrorType::Throw(_)) {
            return Err(err);
        }
        while let Some(handler) = forms.next()? {
            match handler.untag(cx) {
                ObjectType::Cons(cons) => {
                    // Check that conditions match
                    let condition = cons.car();
                    match condition.untag() {
                        ObjectType::Symbol(sym::ERROR | sym::VOID_VARIABLE) => {}
                        // TODO: Remove this once error handling is correctly implemented
                        ObjectType::Symbol(s) if s.name() == "cl--generic-cyclic-definition" => {}
                        ObjectType::Cons(conditions) => {
                            for condition in conditions {
                                let condition = condition?;
                                // TODO: Handle different error symbols
                                if condition != sym::DEBUG && condition != sym::ERROR {
                                    bail_err!("non-error conditions {condition} not yet supported")
                                }
                            }
                        }
                        _ => bail_err!("Invalid condition handler: {condition}"),
                    }

                    // Call handlers with error
                    let error = match err.error {
                        ErrorType::Signal(payload) => payload.condition(cx),
                        ErrorType::Err(err) => {
                            let err_str = format!("{err}");
                            if let Ok(lisp_error) = err.downcast::<LispError>() {
                                lisp_error.bind(cx)
                            } else {
                                // TODO: Need to remove the anyhow branch once
                                // full errors are implemented
                                Cons::new(sym::ERROR, err_str, cx)
                            }
                        }
                        _ => unreachable!("Error type throw was not handled"),
                    };

                    let list: List = match cons.cdr().try_into() {
                        Ok(x) => x,
                        Err(_) => return Ok(NIL),
                    };
                    let previous_bindings = self.vars.len();
                    let binding = Cons::new(var, error, cx);
                    self.vars.push(binding);
                    rooted_iter!(handlers, list, cx);
                    // Errors and throws from the handler must unwind its error
                    // variable too. Do not propagate with `?` before cleanup.
                    let result = self.implicit_progn(handlers, cx);
                    self.vars.truncate(previous_bindings);
                    return result;
                }
                ObjectType::NIL => {}
                invalid => bail_err!("Invalid condition handler: {invalid}"),
            }
        }
        Err(err)
    }
}

pub(crate) fn call_closure<'ob>(
    closure: &Rto<Gc<&Cons>>,
    arg_cnt: usize,
    name: &str,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> EvalResult<'ob> {
    cx.garbage_collect(false);
    let closure: &Cons = closure.untag(cx);
    match closure.car().untag() {
        ObjectType::Symbol(sym::CLOSURE) => {
            rooted_iter!(forms, closure.cdr(), cx);
            let args = Rt::bind_slice(&env.stack[..arg_cnt], cx);
            let vars = bind_variables(&mut forms, args, name, cx)?;
            debug!("call vars: {vars:?}");
            root!(vars, cx);
            Interpreter { vars, env }.implicit_progn(forms, cx)
        }
        other => Err(TypeError::new(Type::Func, other).into()),
    }
}

fn bind_variables<'a>(
    forms: &mut ElemStreamIter<'_>,
    args: &[Object<'a>],
    name: &str,
    cx: &'a Context,
) -> AnyResult<Vec<&'a Cons>> {
    // Add closure environment to variables
    // (closure ((x . 1) (y . 2) t) ...)
    //          ^^^^^^^^^^^^^^^^^^^
    let Some(env) = forms.next()? else { bail!("Closure missing environment") };
    let mut vars = parse_closure_env(env.bind(cx))?;

    // Add function arguments to variables
    // (closure (t) (x y &rest z) ...)
    //              ^^^^^^^^^^^^^
    let Some(arg_list) = forms.next()? else { bail!("Closure missing argument list") };
    bind_args(arg_list.bind(cx), args, &mut vars, name, cx)?;
    Ok(vars)
}

fn parse_closure_env(obj: Object<'_>) -> AnyResult<Vec<&Cons>> {
    let forms = obj.as_list()?;
    let mut env = Vec::new();
    for form in forms {
        match form?.untag() {
            ObjectType::Cons(pair) => {
                env.push(pair);
            }
            ObjectType::TRUE => break,
            x => bail!("Invalid closure environment member: {x}"),
        }
    }
    // The highest priority bindings are at the start of the closure list, but
    // the end of the enviroment vector
    env.reverse();
    Ok(env)
}

fn bind_args<'a>(
    arg_list: Object,
    args: &[Object<'a>],
    vars: &mut Vec<&'a Cons>,
    name: &str,
    cx: &'a Context,
) -> AnyResult<()> {
    let (required, optional, rest) = parse_arg_list(arg_list)?;

    // Formal lists are Lisp data and need not fit native frame metadata.
    // Validate before argument unwraps or publishing any lexical bindings.
    let num_required_args = u16::try_from(required.len())
        .context("Required formal parameter count exceeds supported range")?;
    let num_optional_args = u16::try_from(optional.len())
        .context("Optional formal parameter count exceeds supported range")?;
    let max_args = num_required_args
        .checked_add(num_optional_args)
        .context("Combined formal parameter count exceeds supported range")?;
    let num_actual_args =
        u16::try_from(args.len()).context("Actual argument count exceeds supported range")?;
    // Ensure the minimum number of arguments is present
    ensure!(
        num_actual_args >= num_required_args,
        LispError::arg_cnt(name, num_required_args, num_actual_args, cx)
    );

    let mut arg_values = args.iter().copied();
    let rest_offset = args.len().min(usize::from(max_args));

    for name in required {
        let val = arg_values.next().unwrap();
        vars.push(Cons::new(name, val, cx));
    }

    for name in optional {
        let val = arg_values.next().unwrap_or_default();
        vars.push(Cons::new(name, val, cx));
    }

    if let Some(rest_name) = rest {
        let list = crate::fns::slice_into_list(&args[rest_offset..], None, cx);
        vars.push(Cons::new(rest_name, list, cx));
    } else {
        // Ensure too many args were not provided
        ensure!(
            arg_values.next().is_none(),
            LispError::arg_cnt(name, max_args, num_actual_args, cx)
        );
    }
    Ok(())
}

pub(crate) fn parse_arg_list(
    bindings: Object,
) -> AnyResult<(Vec<Symbol>, Vec<Symbol>, Option<Symbol>)> {
    let mut required = Vec::new();
    let mut optional = Vec::new();
    let mut rest = None;
    let mut arg_type = &mut required;
    let mut iter = bindings.as_list()?;
    while let Some(binding) = iter.next() {
        let sym: Symbol = binding?.try_into()?;
        match sym {
            sym::AND_OPTIONAL => arg_type = &mut optional,
            sym::AND_REST => {
                if let Some(last) = iter.next() {
                    rest = Some(last?.try_into()?);
                    ensure!(iter.next().is_none(), "Found multiple arguments after &rest");
                }
            }
            _ => {
                arg_type.push(sym);
            }
        }
    }
    Ok((required, optional, rest))
}

#[cfg(test)]
pub(crate) fn assert_lisp(compare: &str, expect: &str) {
    let roots = &crate::core::gc::RootSet::default();
    let cx = &mut Context::new(roots);
    sym::init_symbols();
    root!(env, init(Env::new(cx).unwrap()), cx);
    println!("Test String: {compare}");
    let compare = {
        let obj = crate::reader::read(compare, cx).unwrap().0;
        root!(obj, cx);
        rebind!(eval(obj, None, env, cx).unwrap())
    };
    let expect = crate::reader::read(expect, cx).unwrap().0;
    assert_eq!(compare, expect);
}

#[cfg(test)]
mod test {
    use crate::core::{env::intern, gc::RootSet, object::IntoObject};
    use rune_core::macros::list;

    use super::*;

    fn check_interpreter<T>(test_str: &str, expect: T, cx: &mut Context)
    where
        T: IntoObject,
    {
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        println!("Test String: {test_str}");
        let obj = crate::reader::read(test_str, cx).unwrap().0;
        root!(obj, cx);
        let compare = rebind!(eval(obj, None, env, cx).unwrap());
        let expect: Object = expect.into_obj(cx).as_obj_copy();
        assert_eq!(compare, expect);
    }

    #[test]
    fn recovered_save_excursion_semantics() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        for (source, expected) in [
            (
                r#"(progn (insert "aé🙂z") (goto-char 3) (save-excursion (goto-char 1) (insert "λ")) (point))"#,
                4,
            ),
            (
                r#"(progn (insert "aé🙂z") (goto-char 3) (save-excursion (insert "λ")) (point))"#,
                3,
            ),
            (
                r#"(progn (insert "aé🙂z") (goto-char 3) (save-excursion (delete-region 1 4)) (point))"#,
                1,
            ),
            (
                r#"(progn (insert "abcdef") (goto-char 4) (save-excursion (goto-char 2) (save-excursion (goto-char 1) (insert "λ"))) (point))"#,
                5,
            ),
            (
                r#"(progn (insert "abcd") (goto-char 3) (save-excursion (goto-char 1) (upcase-word 1)) (point))"#,
                3,
            ),
            (
                r#"(progn (insert "abcd") (goto-char 3) (condition-case nil (save-excursion (goto-char 1) (if)) (error nil)) (point))"#,
                3,
            ),
            (
                r#"(progn (insert "abcd") (goto-char 3) (catch 'tag (save-excursion (goto-char 1) (throw 'tag 9))) (point))"#,
                3,
            ),
            (r#"(save-excursion (kill-buffer) 7)"#, 7),
        ] {
            check_interpreter(source, expected, cx)
        }
        check_interpreter("(progn (kill-buffer) (save-excursion 1))", 1, cx);
    }
    #[test]
    fn recovered_save_excursion_gc_cleanup() {
        for source in [
            r#"(progn (insert "aé🙂z") (goto-char 3) (save-excursion (goto-char 1) (garbage-collect) (insert "λ") (garbage-collect)) (point))"#,
            r#"(progn (insert "abcd") (goto-char 3) (condition-case nil (save-excursion (goto-char 1) (garbage-collect) (if)) (error nil)) (+ 1 (point)))"#,
            r#"(progn (insert "abcd") (goto-char 3) (+ 1 (length (catch 'tag (save-excursion (goto-char 1) (garbage-collect) (throw 'tag (list 1 2 3)))))))"#,
        ] {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            sym::init_symbols();
            root!(env, init(Env::new(cx).unwrap()), cx);
            let form = crate::reader::read(source, cx).unwrap().0;
            root!(form, cx);
            assert_eq!(eval(form, None, env, cx).unwrap(), Object::from(4_i64));
            assert_eq!(env.buffer_read(cx).unwrap().document.marker_count(), 0);
            cx.garbage_collect(true);
            assert_eq!(env.buffer_read(cx).unwrap().document.marker_count(), 0)
        }
    }
    #[test]
    fn recovered_save_excursion_cross_buffer_identity() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        check_interpreter(
            r#"(let ((saved (current-buffer)) (other (get-buffer-create "recovered-nested-other"))) (insert "aé🙂z") (goto-char 3) (set-buffer other) (insert "efgh") (goto-char 2) (set-buffer saved) (save-excursion (set-buffer other) (save-excursion (goto-char 1) (insert "x")) (set-buffer saved) (goto-char 1) (insert "λ") (set-buffer other)) (+ (point) (progn (set-buffer other) (point))))"#,
            7,
            cx,
        );
        check_interpreter(
            r#"(let ((saved (current-buffer)) (other (get-buffer-create "recovered-killed-saved-other"))) (save-excursion (set-buffer other) (kill-buffer saved)) (eq (current-buffer) other))"#,
            true,
            cx,
        );
    }

    fn check_error(test_str: &str, cx: &mut Context) {
        root!(env, init(Env::new(cx).unwrap()), cx);
        println!("Test String: {test_str}");
        let obj = crate::reader::read(test_str, cx).unwrap().0;
        root!(obj, cx);
        assert!(eval(obj, None, env, cx).is_err());
    }

    #[test]
    fn defconst_reinitializes_value_without_freezing_function_cell() {
        for (source, expected) in [
            ("(defconst vm-dc-result 3)", "vm-dc-result"),
            (
                "(progn (fset 'vm-dc-function (function (lambda () 8))) (defconst vm-dc-function 2) (list vm-dc-function (vm-dc-function)))",
                "(2 8)",
            ),
            (
                "(progn (defconst vm-dc-reset 1) (setq vm-dc-reset 2) (defconst vm-dc-reset 3) vm-dc-reset)",
                "3",
            ),
            (
                "(progn (defconst vm-dc-dynamic 1) (list (let ((vm-dc-dynamic 2)) (defconst vm-dc-dynamic 3) vm-dc-dynamic) vm-dc-dynamic))",
                "(3 1)",
            ),
            ("(progn (defconst vm-dc-mutable 1) (setq vm-dc-mutable 4) vm-dc-mutable)", "4"),
            ("(progn (defconst vm-dc-doc 1 (error \"unevaluated\")) vm-dc-doc)", "1"),
            (
                "(progn (setq vm-dc-side 0) (condition-case nil (defconst vm-dc-extra (setq vm-dc-side 1) \"doc\" extra) (error nil)) vm-dc-side)",
                "0",
            ),
            ("(condition-case nil (defconst vm-dc-missing) (error 'caught))", "caught"),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn dynamic_integer_width_guards_preserve_existing_lexical_bindings() {
        for (source, expected) in [
            ("(condition-case nil (let ((integer-width nil)) 1) (error 'caught))", "caught"),
            ("(condition-case nil (let ((integer-width 1.0)) 1) (error 'caught))", "caught"),
            (
                "(condition-case nil (let ((integer-width 9223372036854775808)) 1) (error 'caught))",
                "caught",
            ),
            (
                "(condition-case nil (let ((integer-width -9223372036854775809)) 1) (error 'caught))",
                "caught",
            ),
            (
                "(let ((integer-width 128)) (condition-case nil (setq integer-width nil) (error nil)) integer-width)",
                "128",
            ),
            (
                "(let ((integer-width 128)) (condition-case nil (set 'integer-width 1.0) (error nil)) integer-width)",
                "128",
            ),
            (
                "(let ((integer-width 128)) (condition-case nil (let* ((integer-width 129) (integer-width nil)) 1) (error nil)) integer-width)",
                "128",
            ),
            (
                "(let ((integer-width 128)) (condition-case nil (let ((integer-width nil)) 1) (error nil)) integer-width)",
                "128",
            ),
            (
                "(let ((seen 0)) (condition-case nil (let ((integer-width nil) (x (setq seen 1))) 1) (error nil)) seen)",
                "1",
            ),
            (
                "(let ((seen 0)) (condition-case nil (let* ((integer-width nil) (x (setq seen 1))) 1) (error nil)) seen)",
                "0",
            ),
            (
                "(let ((integer-width -9223372036854775808)) integer-width)",
                "-9223372036854775808",
            ),
            (
                "(let ((integer-width 9223372036854775807)) integer-width)",
                "9223372036854775807",
            ),
            ("(let ((integer-width 128)) (setq integer-width 129) integer-width)", "129"),
            // Preserve existing lexical closure/handler behavior. These match
            // GNU eval with lexical=t, not its distinct dynamic eval mode.
            ("(funcall (function (lambda (integer-width) integer-width)) nil)", "nil"),
            (
                "(funcall (function (lambda (integer-width) (setq integer-width nil) integer-width)) 128)",
                "nil",
            ),
            ("(funcall (function (lambda (&optional integer-width) integer-width)))", "nil"),
            ("(funcall (function (lambda (&rest integer-width) integer-width)) 128)", "(128)"),
            (
                "(condition-case integer-width (if) (error (setq integer-width nil) integer-width))",
                "nil",
            ),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn oversized_formal_parameter_lists_error_before_binding_or_argument_unwrap() {
        for (required, optional) in [(65_536usize, 0usize), (0, 65_536), (32_768, 32_768)] {
            let roots = &RootSet::default();
            let cx = &Context::new(roots);
            let mut names = vec![Object::from(sym::FILL_COLUMN); required];
            if optional != 0 {
                names.push(sym::AND_OPTIONAL.into());
            }
            names.extend(std::iter::repeat_n(Object::from(sym::FILL_COLUMN), optional));
            let parameters = crate::fns::slice_into_list(&names, None, cx);
            root!(parameters, cx);
            let mut variables = Vec::new();
            let error =
                bind_args(parameters.bind(cx).into(), &[], &mut variables, "oversized-formals", cx)
                    .unwrap_err();
            assert!(error.to_string().contains("formal parameter count exceeds supported range"));
            assert!(variables.is_empty());
        }
    }

    #[test]
    fn basic() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("1", 1, cx);
        check_interpreter("1.5", 1.5, cx);
        check_interpreter("nil", false, cx);
        check_interpreter("t", true, cx);
        check_interpreter("\"foo\"", "foo", cx);
        let list = list!(1, 2; cx);
        root!(list, cx);
        check_interpreter("'(1 2)", list, cx);
    }

    #[test]
    fn variables() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("(let ())", false, cx);
        check_interpreter("(let (x) x)", false, cx);
        check_interpreter("(let ((x 1)) x)", 1, cx);
        check_interpreter("(let ((x 1)))", false, cx);
        check_interpreter("(let ((x 1) (y 2)) x y)", 2, cx);
        check_interpreter("(let ((x 1)) (let ((x 3)) x))", 3, cx);
        check_interpreter("(let ((x 1)) (let ((y 3)) x))", 1, cx);
        check_interpreter("(let ((x 1)) (setq x 2) x)", 2, cx);
        check_interpreter("(let* ())", false, cx);
        check_interpreter("(let* ((x 1) (y x)) y)", 1, cx);
    }

    #[test]
    fn m0_let_unwinds_bindings_on_errors_throws_and_partial_initialization() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        for source in [
            "(let ((x 42)) (condition-case nil (let ((x 7)) (if)) (error x)))",
            "(let ((x 42)) (condition-case nil (let* ((x 7) (y (if))) 0) (error x)))",
            "(let ((x 42)) (condition-case nil (let* ((x 7) 1) 0) (error x)))",
            "(let ((x 42)) (progn (catch 1 (let ((x 7)) (throw 1 9))) x))",
            "(progn (defvar m0-let-unwind-dyn 42) (condition-case nil (let ((m0-let-unwind-dyn 7)) (if)) (error m0-let-unwind-dyn)))",
            "(progn (defvar m0-let-unwind-dyn 42) (condition-case nil (let* ((m0-let-unwind-dyn 7) (y (if))) 0) (error m0-let-unwind-dyn)))",
            "(progn (defvar m0-let-unwind-dyn 42) (catch 1 (let ((m0-let-unwind-dyn 7)) (throw 1 9))) m0-let-unwind-dyn)",
        ] {
            check_interpreter(source, 42_i64, cx);
        }
        check_interpreter(r#"(let ((x "kept")) (garbage-collect) x)"#, "kept", cx);
    }

    #[test]
    fn m0_let_dynamic_count_overflow_is_checked_before_binding() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        root!(env, init(Env::new(cx).unwrap()), cx);
        root!(vars, new(Vec<Slot<&Cons>>), cx);
        let variable = intern("m0-let-binding-count", cx);
        variable.make_special();
        let mut interpreter = Interpreter { vars, env };
        let mut count = u16::MAX;
        assert!(interpreter.create_let_binding(variable, 7_i64.into(), &mut count, cx).is_err());
        assert_eq!(count, u16::MAX);
        assert!(interpreter.env.vars.get(variable).is_none());
    }

    #[test]
    fn dyn_variables() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("(progn (defvar dyn_test1 1) dyn_test1)", 1, cx);
        check_interpreter("(progn (defvar dyn_test2 1) (let ((dyn_test2 3)) dyn_test2))", 3, cx);
        check_interpreter("(progn (defvar dyn_test3 1) (let ((dyn_test3 3))) dyn_test3)", 1, cx);
        check_interpreter("(let ((dyn_test4 7)) (defvar dyn_test4 3) dyn_test4)", 7, cx);
        check_interpreter(
            "(progn (defvar dyn_test5 1) (let (bar) (let ((dyn_test5 3)) (setq bar dyn_test5)) bar))",
            3,
            cx,
        );
        // Special but unbound
        check_interpreter(
            "(progn (defvar dyn_test6 1) (makunbound 'dyn_test6) (let ((fn #'(lambda () (defvar dyn_test6 3))) (dyn_test6 7)) (funcall fn)) dyn_test6)",
            3,
            cx,
        );
        check_interpreter(
            "(progn (defvar dyn_test7 1) (makunbound 'dyn_test7) (let ((dyn_test7 7)) dyn_test7))",
            7,
            cx,
        );
        check_interpreter("(eq (make-symbol \"bar\") 'bar)", false, cx);
        check_interpreter(
            "(let ((x (make-symbol \"x\"))) (put x 'p t) (garbage-collect) (get x 'p))",
            true,
            cx,
        );
    }

    #[test]
    fn conditionals() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("(if nil 1)", false, cx);
        check_interpreter("(if t 1)", 1, cx);
        check_interpreter("(if nil 1 2)", 2, cx);
        check_interpreter("(if t 1 2)", 1, cx);
        check_interpreter("(if nil 1 2 3)", 3, cx);
        check_interpreter("(and)", true, cx);
        check_interpreter("(and 1)", 1, cx);
        check_interpreter("(and 1 2)", 2, cx);
        check_interpreter("(and 1 nil)", false, cx);
        check_interpreter("(and nil 1)", false, cx);
        check_interpreter("(or)", false, cx);
        check_interpreter("(or nil)", false, cx);
        check_interpreter("(or nil 1)", 1, cx);
        check_interpreter("(or 1 2)", 1, cx);
        check_interpreter("(cond)", false, cx);
        check_interpreter("(cond nil)", false, cx);
        check_interpreter("(cond (1))", 1, cx);
        check_interpreter("(cond (1 2))", 2, cx);
        check_interpreter("(cond (nil 1) (2 3))", 3, cx);
        check_interpreter("(cond (nil 1) (2 3) (4 5))", 3, cx);
    }

    #[test]
    fn test_loops() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter(
            "(let ((i 3) (x 0)) (while (> i 0) (setq x (+ x i) i (1- i) )) x)",
            6,
            cx,
        );
        check_interpreter(
            "(let ((i 3) (x 0)) (while (progn (setq x (1- x)) (> i 0)) (setq x (+ x i) i (1- i) )) x)",
            2,
            cx,
        );
    }

    #[test]
    fn special_forms() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("(prog1 1 2 3)", 1, cx);
        check_interpreter("(prog2 1 2 3)", 2, cx);
        check_interpreter("(progn 1 2 3 4)", 4, cx);
        check_interpreter("(function 1)", 1, cx);
        check_interpreter("(quote 1)", 1, cx);
        check_interpreter("(if 1 2 3)", 2, cx);
        check_interpreter("(if nil 2 3)", 3, cx);
        check_interpreter("(if (and 1 nil) 2 3)", 3, cx);
    }

    #[test]
    fn test_functions() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        let list = list![sym::CLOSURE, list![true; cx]; cx];
        root!(list, cx);
        check_interpreter("(function (lambda))", list, cx);
        let x = intern("x", cx);
        let list = list![sym::CLOSURE, list![true; cx], list![x; cx], x; cx];
        root!(list, cx);
        check_interpreter("(function (lambda (x) x))", list, cx);
        // TODO: fix this duplicate intern
        let x = intern("x", cx);
        let y = intern("y", cx);
        let list: Object =
            list![sym::CLOSURE, list![Cons::new(y, 1, cx), true; cx], list![x; cx], x; cx];
        root!(list, cx);
        check_interpreter("(let ((y 1)) (function (lambda (x) x)))", list, cx);

        let list = list!(5, false; cx);
        root!(list, cx);
        check_interpreter(
            "(let ((x #'(lambda (x &optional y &rest z) (cons x (cons y z))))) (funcall x 5))",
            list,
            cx,
        );
        let list = list!(5, 7; cx);
        root!(list, cx);
        check_interpreter(
            "(let ((x #'(lambda (x &optional y &rest z) (cons x (cons y z))))) (funcall x 5 7))",
            list,
            cx,
        );
        let list = list!(5, 7, 11; cx);
        root!(list, cx);
        check_interpreter(
            "(let ((x #'(lambda (x &optional y &rest z) (cons x (cons y z))))) (funcall x 5 7 11))",
            list,
            cx,
        );
        check_interpreter(
            "(let (z) (setq z (let ((x 5)) (let ((x 3)) (function (lambda () x))))) (funcall z))",
            3,
            cx,
        );
    }

    #[test]
    fn test_call() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("(let ((x #'(lambda (x) x))) (funcall x 5))", 5, cx);
        check_interpreter("(let ((x #'(lambda () 3))) (funcall x))", 3, cx);
        check_interpreter(
            "(progn (defvar foo 1) (let ((x #'(lambda () foo)) (foo 5)) (funcall x)))",
            5,
            cx,
        );
        check_interpreter(
            "(progn (defalias 'int-test-call #'(lambda (x) (+ x 3)))  (int-test-call 7))",
            10,
            cx,
        );
        // Test closures
        check_interpreter("(let* ((y 7)(x #'(lambda () y))) (funcall x))", 7, cx);
        check_interpreter("(let* ((y 7)(x #'(lambda (x) (+ x y)))) (funcall x 3))", 10, cx);
        // Test that closures capture their environments
        check_interpreter(
            "(progn (setq func (let ((x 3)) #'(lambda (y) (+ y x)))) (funcall func 5))",
            8,
            cx,
        );
        // Test multiple closures
        check_interpreter(
            "(progn (setq funcs (let ((x 3)) (cons #'(lambda (y) (+ y x)) #'(lambda (y) (- y x))))) (* (funcall (car funcs) 5) (funcall (cdr funcs) 1)))",
            -16,
            cx,
        );
        // Test that closures close over variables
        check_interpreter(
            "(progn (setq funcs (let ((x 3)) (cons #'(lambda (y) (setq x y)) #'(lambda (y) (+ y x))))) (funcall (car funcs) 5) (funcall (cdr funcs) 4))",
            9,
            cx,
        );
        // Test that closures in global function close over values and not
        // variables
        check_interpreter(
            "(progn (setq func (let ((x 3)) (defalias 'int-test-no-cap #'(lambda (y) (+ y x))) #'(lambda (y) (setq x y)))) (funcall func 4) (int-test-no-cap 5))",
            8,
            cx,
        );

        // takes 1 arg
        check_error("(1+)", cx);
        check_error("(/)", cx);
        check_error("(1+ 1 2)", cx);
    }

    #[test]
    fn test_condition_case() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("(condition-case nil nil)", false, cx);
        check_interpreter("(condition-case nil 1)", 1, cx);
        check_interpreter("(condition-case nil (if) (error 7))", 7, cx);
        check_interpreter("(condition-case nil (if) (error 7 9 11))", 11, cx);
        check_interpreter("(condition-case nil (if) (error . 7))", false, cx);
        check_interpreter("(condition-case nil (if) ((debug error) 7))", 7, cx);
        // Ensure that errors don't get garbage collected
        check_interpreter(
            "(condition-case e (if t) (error (progn (garbage-collect) (nth 2 e))))",
            2,
            cx,
        );
        check_error("(condition-case nil (if))", cx);
        check_error("(condition-case nil (if) nil)", cx);
        check_error("(condition-case nil (if) 5 (error 7))", cx);
    }

    #[test]
    fn m0_condition_case_unwinds_handler_variable_on_error_and_throw() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter(
            "(let ((e 42)) (condition-case nil (condition-case e (if) (error (if))) (error e)))",
            42,
            cx,
        );
        check_interpreter(
            "(let ((e 42)) (progn (catch 'tag (condition-case e (if) (error (throw 'tag 9)))) e))",
            42,
            cx,
        );
        check_interpreter(
            "(let ((e 42)) (condition-case nil (condition-case e (if) (error (garbage-collect) (if))) (error e)))",
            42,
            cx,
        );
        // An invalid dotted handler body takes an early return but must never
        // install a handler-variable binding visible to the following form.
        check_interpreter("(let ((e 42)) (progn (condition-case e (if) (error . 7)) e))", 42, cx);
    }

    #[test]
    fn m0_catch_unwinds_tags_on_all_exit_paths() {
        for (source, expected) in [
            ("(catch 1 7)", Some(7_i64)),
            ("(catch 1 (throw 1 2))", Some(2)),
            ("(catch 1 (catch 2 (throw 1 3)))", Some(3)),
            ("(catch 1 (if))", None),
            ("(catch (if) 7)", None),
            ("(catch 1 (throw (if) 9))", None),
            ("(catch 1 (throw 1 (if)))", None),
        ] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            root!(env, init(Env::new(cx).unwrap()), cx);
            let form = crate::reader::read(source, cx).unwrap().0;
            root!(form, cx);
            let result = eval(form, None, env, cx);
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), Object::from(expected)),
                None => assert!(result.is_err()),
            }
            assert!(env.catch_stack.is_empty(), "leaked catch tag after {source}");
            // Reusing the same environment must not find the expired catch.
            let form = crate::reader::read("(throw 1 9)", cx).unwrap().0;
            root!(form, cx);
            let error = eval(form, None, env, cx).unwrap_err();
            let error = error.downcast::<EvalError>().unwrap();
            assert!(!matches!(error.error, ErrorType::Throw(_)));
            assert!(env.catch_stack.is_empty());
        }
    }

    #[test]
    fn catch_and_throw_evaluate_tags_and_values_once_in_order() {
        assert_lisp("(let ((tag 'target)) (catch tag (throw tag (+ 2 3))))", "5");
        assert_lisp(
            "(let ((n 0)) (catch (progn (setq n (+ n 1)) 'target) (throw (progn (setq n (+ n 1)) 'target) (progn (setq n (+ n 1)) n))))",
            "3",
        );
        assert_lisp(
            "(let ((n 0)) (condition-case nil (throw 'absent (progn (setq n (+ n 1)) 7)) (error n)))",
            "1",
        );
        assert_lisp("(catch 'target (throw 'target 7))", "7");
    }

    #[test]
    fn throw_validates_arity_before_effects_and_nested_exits_take_priority() {
        assert_lisp("(let ((n 0)) (condition-case nil (throw (setq n 1)) (error n)))", "0");
        assert_lisp(
            "(let ((n 0)) (condition-case nil (throw (setq n 1) (setq n 2) (setq n 3)) (error n)))",
            "0",
        );
        assert_lisp("(catch 'tag (throw 'tag (throw 'tag 11)))", "11");
        assert_lisp("(catch 'outer (catch (throw 'outer 7) 9))", "7");
        assert_lisp("(catch 'outer (throw (throw 'outer 7) 9))", "7");
    }

    #[test]
    fn catch_matching_uses_identity_and_evaluated_tags_survive_collection() {
        assert_lisp("(let ((tag (list 1))) (catch tag (garbage-collect) (throw tag 7)))", "7");
        assert_lisp(
            "(let ((tag (list 1))) (catch tag (throw (progn (garbage-collect) tag) (progn (garbage-collect) 7))))",
            "7",
        );
        assert_lisp("(condition-case nil (catch (list 1) (throw (list 1) 7)) (error 9))", "9");
    }

    #[test]
    fn cyclic_native_exit_values_preserve_identity_without_recursive_diagnostics() {
        assert_lisp(
            "(let ((tag (list 17)) (value (list 7))) (setcdr tag tag) (setcdr value value) (eq (catch tag (throw tag value)) value))",
            "t",
        );
        assert_lisp(
            "(let ((value (list 7))) (setcdr value value) (eq (condition-case e (signal 'error value) (error (cdr e))) value))",
            "t",
        );
        assert_lisp(
            "(let ((tag (list 17))) (setcdr tag tag) (condition-case nil (throw tag 7) (error 9)))",
            "9",
        );
    }

    #[test]
    fn pending_exits_survive_cleanup_local_handled_exits() {
        assert_lisp(
            "(catch 'outer (unwind-protect (throw 'outer 1) (catch 'inner (throw 'inner 2))))",
            "1",
        );
        assert_lisp(
            "(condition-case e (unwind-protect (signal 'error '(1)) (condition-case nil (signal 'error '(2)) (error nil))) (error (car (cdr e))))",
            "1",
        );
        assert_lisp(
            "(let ((tag (list 17)) (value (list 7))) (eq (catch tag (unwind-protect (throw tag value) (catch 'inner (throw 'inner 2)) (garbage-collect))) value))",
            "t",
        );
        assert_lisp("(catch 'outer (unwind-protect (throw 'outer 1) (throw 'outer 3)))", "3");
    }

    #[test]
    fn throw_is_an_ordinary_callable_native_function() {
        assert_lisp("(functionp 'throw)", "t");
        assert_lisp("(subrp (symbol-function 'throw))", "t");
        assert_lisp("(func-arity 'throw)", "(2 . 2)");
        assert_lisp("(catch 17 (funcall 'throw 17 7))", "7");
        assert_lisp("(catch 17 (apply 'throw '(17 7)))", "7");
        assert_lisp("(condition-case e (throw 1) (error (eq (car (cdr e)) 'throw)))", "t");
        for source in [
            "(let ((n 0)) (condition-case nil (car (setq n 1) 2) (error n)))",
            "(let ((n 0)) (condition-case nil (cons (setq n 1)) (error n)))",
            "(let ((n 0)) (condition-case nil (identity (setq n 1) 2) (error n)))",
            "(let ((n 0)) (condition-case nil (aref (setq n 1)) (error n)))",
            "(let ((n 0)) (condition-case nil (stringp (setq n 1) 2) (error n)))",
            "(let ((n 0)) (condition-case nil (car (setq n 1) . 2) (error n)))",
        ] {
            assert_lisp(source, "0");
        }
        assert_lisp(
            "(let ((n 0)) (condition-case nil (funcall 'throw (setq n 1)) (error n)))",
            "1",
        );
    }

    #[test]
    fn test_throw_catch() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        check_interpreter("(catch nil)", false, cx);
        check_interpreter("(catch nil nil)", false, cx);
        check_interpreter("(catch 1 (throw 1 2))", 2, cx);
        check_interpreter("(catch 1 (throw 1 2) 3)", 2, cx);
        check_interpreter("(catch 1 5 (throw 1 2) 3)", 2, cx);
        check_interpreter("(catch 1 (throw 1 2) (if))", 2, cx);
        check_interpreter("(condition-case nil (throw 1 2) (error 3))", 3, cx);
        check_interpreter("(catch 1 (condition-case nil (throw 1 2) (error 3)))", 2, cx);
        check_interpreter("(catch 1 (catch 2 (throw 1 3)))", 3, cx);
        check_error("(throw 1 2)", cx);
        check_error("(catch 2 (throw 3 4))", cx);
    }
}
