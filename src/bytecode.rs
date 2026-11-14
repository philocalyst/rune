//! The main bytecode interpeter.
use crate::core::cons::Cons;
use crate::core::env::{CallFrame, Env, sym};
use crate::core::gc::{Context, IntoRoot, Rt, Rto, Slot};
use crate::core::object::{
    ByteFn, ByteString, FnArgs, Function, FunctionType, Gc, LispVec, NIL, Object, ObjectType,
    Symbol, WithLifetime,
};
use crate::data::LispError;
use crate::eval::{ErrorType, EvalError, EvalResult};
use anyhow::{Result, bail, ensure};
use rune_core::macros::{bail_err, rebind, root};
use rune_macros::{Trace, defun};

mod opcode;

mod decoder;
use decoder::{ProgramCache, ProgramCounter};

#[derive(Debug, Trace)]
/// A handler for a condition-case. These are stored in a vector in the VM and
/// added/removed via bytecodes.
struct Handler<'ob> {
    #[no_trace]
    jump_code: u16,
    #[no_trace]
    stack_size: usize,
    #[no_trace]
    stack_frame: usize,
    #[no_trace]
    binding_depth: usize,
    #[no_trace]
    binding_frames: usize,
    #[no_trace]
    is_catch: bool,
    #[no_trace]
    catch_depth: usize,
    condition: Slot<Object<'ob>>,
}

impl<'new> IntoRoot<Handler<'new>> for Handler<'_> {
    unsafe fn into_root(self) -> Handler<'new> {
        self.with_lifetime()
    }
}

impl<'old, 'new> WithLifetime<'new> for Handler<'old> {
    type Out = Handler<'new>;

    unsafe fn with_lifetime(self) -> Self::Out {
        std::mem::transmute::<Handler<'old>, Handler<'new>>(self)
    }
}

#[derive(Clone, Copy)]
struct FrameCheckpoint {
    binding_depth: usize,
    catch_depth: usize,
}

/// The bytecode VM. This hold all the current call frames and handlers. The
/// execution stack is part of the Environment.
#[derive(Trace)]
struct VM<'brw, 'env, 'rt> {
    #[no_trace]
    /// Current program counter
    pc: ProgramCounter,
    #[no_trace]
    programs: ProgramCache,
    #[no_trace]
    binding_frames: Vec<FrameCheckpoint>,
    /// The current function being executed. Saved to ensure it is preserved by
    /// the garbage collector.
    func: Slot<&'rt ByteFn>,
    /// All currently active condition-case handlers
    handlers: Vec<Handler<'rt>>,
    /// The runtime environment
    #[no_trace]
    env: &'brw mut Rt<Env<'env>>,
}

impl<'brw, 'env> IntoRoot<VM<'brw, 'env, 'static>> for VM<'brw, 'env, '_> {
    unsafe fn into_root(self) -> VM<'brw, 'env, 'static> {
        std::mem::transmute(self)
    }
}

defsym!(RUNE_UNIMPLEMENTED);

impl<'ob> RootedVM<'_, '_, '_> {
    /// Unsupported operations must cross the normal Lisp error boundary, not
    /// unwind the Rust process. Opcode semantics remain deliberately unimplemented.
    fn unsupported_opcode(&mut self, opcode: opcode::OpCode, cx: &Context) -> EvalError {
        let data = Cons::new1(cx.add(format!("Unsupported bytecode opcode: {opcode:?}")), cx);
        EvalError::signal(sym::RUNE_UNIMPLEMENTED.into(), data.into(), self.env)
    }

    fn require_stack(&self, count: usize) -> Result<()> {
        ensure!(
            count <= self.env.stack.frame_len(),
            "Bytecode operand stack underflow: need {count} values"
        );
        Ok(())
    }

    fn stack_ref(&mut self, idx: u16, cx: &Context) -> Result<()> {
        ensure!(
            usize::from(idx) < self.env.stack.frame_len(),
            "Bytecode stack reference out of range"
        );
        ensure!(self.env.stack.can_push(), "Bytecode stack depth exceeded");
        self.env.stack.push_ref(idx, cx);
        Ok(())
    }

    fn stack_set(&mut self, idx: u16) -> Result<()> {
        ensure!(
            usize::from(idx) < self.env.stack.frame_len(),
            "Bytecode stack assignment out of range"
        );
        self.env.stack.set_ref(idx);
        Ok(())
    }

    fn check_instruction_stack(&self, instruction: decoder::Instruction) -> Result<()> {
        if let Some(effect) = instruction.stack_effect() {
            self.require_stack(effect.inputs)?;
            // GNU instructions grow by at most one operand value. Indexed
            // stack references count retained operands in their effect.
            if effect.outputs > effect.inputs {
                ensure!(self.env.stack.can_push(), "Bytecode stack depth exceeded");
            }
        }
        Ok(())
    }

    fn concatenate(&mut self, count: usize, cx: &Context) -> Result<()> {
        self.require_stack(count)?;
        if count == 0 {
            ensure!(self.env.stack.can_push(), "Bytecode stack depth exceeded");
        }
        let arguments = Rt::bind_slice(&self.env.stack[..count], cx);
        let result = cx.add(crate::fns::concat(arguments)?);
        let length = self.env.stack.len();
        self.env.stack.truncate(length - count);
        self.env.stack.push(result);
        Ok(())
    }

    fn varref(&mut self, idx: u16, cx: &'ob Context) -> Result<()> {
        let symbol: Symbol = self.get_const(idx as usize, cx)?.try_into()?;
        let Some(var) = self.env.vars.get(symbol) else { bail!("Void Variable: {symbol}") };
        let var = var.bind(cx);
        self.env.stack.push(var);
        Ok(())
    }

    fn varset(&mut self, idx: usize, cx: &Context) -> Result<()> {
        let obj = self.get_const(idx, cx)?;
        let symbol: Symbol = obj.try_into()?;
        self.require_stack(1)?;
        let value = self.env.stack.pop(cx);
        crate::data::set(symbol, value, self.env)?;
        Ok(())
    }

    fn varbind(&mut self, idx: u16, cx: &'ob Context) -> Result<()> {
        // Validate the constant before consuming a stack value.
        let symbol: Symbol = self.get_const(idx as usize, cx)?.try_into()?;
        self.require_stack(1)?;
        let value = self.env.stack.pop(cx);
        self.env.varbind(symbol, value, cx);
        Ok(())
    }

    fn unbind(&mut self, idx: u16, cx: &'ob Context) -> Result<()> {
        let floor = self.binding_frames.last().expect("VM has a binding frame").binding_depth;
        let depth = self.env.binding_depth();
        ensure!(
            usize::from(idx) <= depth.saturating_sub(floor),
            "Bytecode binding stack underflow"
        );
        self.env.unwind_bindings(depth - usize::from(idx), cx)
    }

    fn get_const(&self, i: usize, cx: &'ob Context) -> Result<Object<'ob>> {
        self.func
            .bind(cx)
            .consts()
            .get(i)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Invalid bytecode constant index: {i}"))
    }

    fn set_current_frame(&mut self, f: &ByteFn, offset: usize) -> Result<()> {
        let cursor = self.programs.cursor(f.codes(), offset)?;
        self.func.set(f);
        self.pc = cursor;
        Ok(())
    }

    fn unwind(&mut self, idx: usize, cx: &'ob Context) -> Result<()> {
        if idx == self.env.stack.current_frame() {
            return Ok(());
        }
        ensure!(idx < self.env.stack.current_frame(), "Invalid bytecode unwind frame");
        if let Some((f, offset)) = self.env.stack.get_bytecode_frame(idx) {
            self.set_current_frame(f.bind(cx), offset)?;
            self.env.stack.unwind_frames(idx);
            Ok(())
        } else {
            bail!("Bytecode unwind frame not found")
        }
    }

    #[inline(always)]
    fn debug_enabled() -> bool {
        cfg!(test) || (cfg!(feature = "debug_bytecode") && crate::debug::debug_enabled())
    }

    /// Prepare the arguments for lisp function call. This means filling all
    /// needed stack slots with `nil` and moving all the `&rest` arguments into
    /// a list.
    fn prepare_lisp_args(
        &mut self,
        func: &ByteFn,
        arg_cnt: usize,
        name: &str,
        cx: &'ob Context,
    ) -> Result<()> {
        let arg_cnt = u16::try_from(arg_cnt)
            .map_err(|_| anyhow::anyhow!("Bytecode argument count exceeds supported range"))?;
        let fill_args = num_of_fill_args(func.args, arg_cnt, name, cx)?;
        let parameter_slots =
            usize::from(func.args.required + func.args.optional) + usize::from(func.args.rest);
        ensure!(parameter_slots <= func.depth, "Bytecode stack depth exceeded by arguments");
        self.env.stack.fill_extra_args(fill_args);
        let total_args = arg_cnt + fill_args;
        let rest_size = total_args - (func.args.required + func.args.optional);
        if rest_size > 0 {
            let slice = &self.env.stack[..rest_size as usize];
            let list = crate::fns::slice_into_list(Rt::bind_slice(slice, cx), None, cx);
            self.env.stack.remove_top(rest_size as usize - 1);
            self.env.stack[0].set(list);
            self.env.stack.set_arg_count(total_args - rest_size + 1, true);
        } else if func.args.rest {
            self.env.stack.push(NIL);
            self.env.stack.set_arg_count(total_args + 1, true)
        } else {
            self.env.stack.set_arg_count(total_args, false)
        };
        Ok(())
    }

    fn call(&mut self, arg_cnt: u16, cx: &'ob mut Context) -> Result<(), EvalError> {
        let arg_cnt = usize::from(arg_cnt);
        self.require_stack(arg_cnt + 1)?;
        let func: Function = self.env.stack[arg_cnt].bind(cx).try_into()?;
        let name = match func.untag() {
            FunctionType::Symbol(x) => x.name().to_owned(),
            _ => String::from("lambda"),
        };
        if let FunctionType::ByteFn(next_fn) = func.untag() {
            // If bytecode, add another frame and resume execution.
            // OpCode::Return will remove the call frame.
            let len = self.env.stack.len();
            let pc_offset = self.pc.as_offset();
            let prev_fn = self.func.bind(cx);
            let cursor = self.programs.cursor(next_fn.codes(), 0).map_err(anyhow::Error::from)?;
            self.binding_frames.try_reserve(1).map_err(|error| {
                anyhow::anyhow!("Cannot reserve bytecode binding frame: {error}")
            })?;
            let frame_start = len - (arg_cnt + 1);
            self.env.stack.push_bytecode_frame(
                frame_start,
                next_fn.depth,
                prev_fn,
                pc_offset,
                cx,
            )?;
            // All checked frame reservations succeeded. Publishing the new
            // code/root/checkpoint now cannot expose a half-entered callee.
            self.func.set(next_fn);
            self.pc = cursor;
            self.binding_frames.push(FrameCheckpoint {
                binding_depth: self.env.binding_depth(),
                catch_depth: self.env.catch_stack.len(),
            });
            self.prepare_lisp_args(next_fn, arg_cnt, &name, cx)?;
        } else {
            // Otherwise, call the function directly.
            let mut frame = CallFrame::new_with_args(self.env, arg_cnt);
            root!(func, cx);
            let result = func.call(&mut frame, Some(&name), cx)?;
            drop(frame); // removes the arguments from the stack
            self.env.stack.top().set(result);
            cx.garbage_collect(false);
        }
        Ok(())
    }

    fn resume_handler(
        &mut self,
        handler: &Handler<'_>,
        value: Object<'ob>,
        cx: &'ob Context,
    ) -> Result<()> {
        self.env.unwind_bindings(handler.binding_depth, cx)?;
        self.env.catch_stack.truncate(handler.catch_depth);
        self.binding_frames.truncate(handler.binding_frames);
        self.unwind(handler.stack_frame, cx)?;
        self.env.stack.truncate(handler.stack_size);
        self.env.stack.push(value);
        self.pc.goto(usize::from(handler.jump_code)).map_err(anyhow::Error::from)?;
        Ok(())
    }

    fn run(&mut self, cx: &'ob mut Context) -> EvalResult<'ob> {
        'main: loop {
            let err = match self.execute_bytecode(cx) {
                Ok(x) => return Ok(rebind!(x, cx)),
                Err(e) => e,
            };

            while let Some(handler) = self.handlers.bind_mut(cx).pop() {
                self.env.catch_stack.truncate(handler.catch_depth);
                if handler.is_catch {
                    let ErrorType::Throw(id) = &err.error else { continue };
                    let Some((tag, value)) = self.env.get_exception(*id) else {
                        bail_err!("Exception not found");
                    };
                    if !handler.condition.ptr_eq(tag.bind(cx)) {
                        continue;
                    }
                    let value = value.bind(cx);
                    self.resume_handler(&handler, value, cx)?;
                    continue 'main;
                }
                // Conditions never intercept a nonlocal throw.
                if matches!(&err.error, ErrorType::Throw(_)) {
                    continue;
                }
                match handler.condition.untag() {
                    ObjectType::Symbol(sym::ERROR) => {}
                    ObjectType::Cons(conditions) => {
                        for condition in conditions {
                            let condition = condition?;
                            // TODO: Handle different error symbols
                            if condition != sym::DEBUG && condition != sym::ERROR {
                                bail_err!("non-error conditions {condition} not yet supported")
                            }
                        }
                    }
                    x => bail_err!("Invalid condition handler: {x}"),
                }

                let rendered = format!("{err}");
                let error = match err.error {
                    ErrorType::Signal(id) => {
                        let Some((sym, data)) = self.env.get_exception(id) else {
                            bail_err!("Exception not found")
                        };
                        Cons::new(sym, data, cx)
                    }
                    ErrorType::Err(error) => {
                        // Preserve the condition symbol and data just as the
                        // interpreter does. LispError owns its GC-safe payload;
                        // bind also handles foreign or retired owner domains.
                        match error.downcast::<LispError>() {
                            Ok(error) => error.bind(cx),
                            Err(_) => Cons::new(sym::ERROR, rendered, cx),
                        }
                    }
                    ErrorType::Throw(_) => Cons::new(sym::ERROR, rendered, cx),
                };
                self.resume_handler(&handler, Object::from(error), cx)?;
                continue 'main;
            }
            return Err(err);
        }
    }

    #[expect(clippy::too_many_lines)]
    /// The main bytecode execution loop.
    fn execute_bytecode(&mut self, cx: &'ob mut Context) -> EvalResult<'ob> {
        use crate::{alloc, arith, data, fns};
        use opcode::OpCode as op;
        loop {
            let instruction = self.pc.fetch().map_err(anyhow::Error::from)?;
            let op = instruction.opcode;
            self.check_instruction_stack(instruction)?;

            if Self::debug_enabled() {
                println!("[");
                for (idx, x) in self.env.stack.frames().iter().rev().enumerate() {
                    println!("    {idx}: {x},");
                }
                println!("]");
                let byte_offset = instruction.offset;
                println!("op :{byte_offset}: {op:?}");
            }
            match op {
                op::StackRef0 => self.stack_ref(0, cx)?,
                op::StackRef1 => self.stack_ref(1, cx)?,
                op::StackRef2 => self.stack_ref(2, cx)?,
                op::StackRef3 => self.stack_ref(3, cx)?,
                op::StackRef4 => self.stack_ref(4, cx)?,
                op::StackRef5 => self.stack_ref(5, cx)?,
                op::StackRefN => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.stack_ref(idx, cx)?;
                }
                op::StackRefN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.stack_ref(idx, cx)?;
                }
                op::StackSetN => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.stack_set(idx)?;
                }
                op::StackSetN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.stack_set(idx)?;
                }
                op::VarRef0 => self.varref(0, cx)?,
                op::VarRef1 => self.varref(1, cx)?,
                op::VarRef2 => self.varref(2, cx)?,
                op::VarRef3 => self.varref(3, cx)?,
                op::VarRef4 => self.varref(4, cx)?,
                op::VarRef5 => self.varref(5, cx)?,
                op::VarRefN => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.varref(idx, cx)?;
                }
                op::VarRefN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.varref(idx, cx)?;
                }
                op::VarSet0 => self.varset(0, cx)?,
                op::VarSet1 => self.varset(1, cx)?,
                op::VarSet2 => self.varset(2, cx)?,
                op::VarSet3 => self.varset(3, cx)?,
                op::VarSet4 => self.varset(4, cx)?,
                op::VarSet5 => self.varset(5, cx)?,
                op::VarSetN => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.varset(idx.into(), cx)?;
                }
                op::VarSetN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.varset(idx.into(), cx)?;
                }
                op::VarBind0 => self.varbind(0, cx)?,
                op::VarBind1 => self.varbind(1, cx)?,
                op::VarBind2 => self.varbind(2, cx)?,
                op::VarBind3 => self.varbind(3, cx)?,
                op::VarBind4 => self.varbind(4, cx)?,
                op::VarBind5 => self.varbind(5, cx)?,
                op::VarBindN => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.varbind(idx, cx)?;
                }
                op::VarBindN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.varbind(idx, cx)?;
                }
                op::Call0 => self.call(0, cx)?,
                op::Call1 => self.call(1, cx)?,
                op::Call2 => self.call(2, cx)?,
                op::Call3 => self.call(3, cx)?,
                op::Call4 => self.call(4, cx)?,
                op::Call5 => self.call(5, cx)?,
                op::CallN => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.call(idx, cx)?;
                }
                op::CallN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.call(idx, cx)?;
                }
                op::Unbind0 => self.unbind(0, cx)?,
                op::Unbind1 => self.unbind(1, cx)?,
                op::Unbind2 => self.unbind(2, cx)?,
                op::Unbind3 => self.unbind(3, cx)?,
                op::Unbind4 => self.unbind(4, cx)?,
                op::Unbind5 => self.unbind(5, cx)?,
                op::UnbindN => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.unbind(idx, cx)?;
                }
                op::UnbindN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    self.unbind(idx, cx)?;
                }
                op::PopHandler => {
                    let frame = self.env.stack.current_frame();
                    if !self
                        .handlers
                        .bind_mut(cx)
                        .last()
                        .is_some_and(|handler| handler.stack_frame == frame)
                    {
                        bail_err!("Bytecode handler stack underflow");
                    }
                    let handler = self.handlers.bind_mut(cx).pop().expect("checked handler");
                    self.env.catch_stack.truncate(handler.catch_depth);
                }
                op::PushCondtionCase | op::PushCatch => {
                    let jump_code = instruction.operand().map_err(anyhow::Error::from)?;
                    self.pc.validate_target(usize::from(jump_code)).map_err(anyhow::Error::from)?;
                    // pop before getting stack size, after validating operands
                    self.require_stack(1)?;
                    let condition = self.env.stack.pop(cx);
                    let catch_depth = self.env.catch_stack.len();
                    let is_catch = op == op::PushCatch;
                    if is_catch {
                        self.env.catch_stack.push(condition);
                    }
                    let handler = Handler {
                        is_catch,
                        catch_depth,
                        jump_code,
                        stack_size: self.env.stack.len(),
                        stack_frame: self.env.stack.current_frame(),
                        binding_depth: self.env.binding_depth(),
                        binding_frames: self.binding_frames.len(),
                        condition: Slot::new(condition),
                    };
                    self.handlers.push(handler);
                }
                op::Nth => {
                    let list = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::nth(top.bind_as(cx)?, list.try_into()?)?);
                }
                op::Symbolp => {
                    let top = self.env.stack.top();
                    top.set(data::symbolp(top.bind(cx)));
                }
                op::Consp => {
                    let top = self.env.stack.top();
                    top.set(data::consp(top.bind(cx)));
                }
                op::Stringp => {
                    let top = self.env.stack.top();
                    top.set(data::stringp(top.bind(cx)));
                }
                op::Listp => {
                    let top = self.env.stack.top();
                    top.set(data::listp(top.bind(cx)));
                }
                op::Eq => {
                    let v1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::eq(top.bind(cx), v1));
                }
                op::Memq => {
                    let list = self.env.stack.pop(cx);
                    let elt = self.env.stack.top();
                    elt.set(fns::memq(elt.bind(cx), list.try_into()?)?);
                }
                op::Not => {
                    let top = self.env.stack.top();
                    top.set(data::null(top.bind(cx)));
                }
                op::Car => {
                    let top = self.env.stack.top();
                    top.set(data::car(top.bind_as(cx)?));
                }
                op::Cdr => {
                    let top = self.env.stack.top();
                    top.set(data::cdr(top.bind_as(cx)?));
                }
                op::Cons => {
                    let cdr = self.env.stack.pop(cx);
                    let car = self.env.stack.top();
                    car.set(data::cons(car.bind(cx), cdr, cx));
                }
                op::List1 => {
                    let top = self.env.stack.top();
                    top.set(alloc::list(&[top.bind(cx)], cx));
                }
                op::List2 => {
                    let a2 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(alloc::list(&[top.bind(cx), a2], cx));
                }
                op::List3 => {
                    let a3 = self.env.stack.pop(cx);
                    let a2 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(alloc::list(&[top.bind(cx), a2, a3], cx));
                }
                op::List4 => {
                    let a4 = self.env.stack.pop(cx);
                    let a3 = self.env.stack.pop(cx);
                    let a2 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(alloc::list(&[top.bind(cx), a2, a3, a4], cx));
                }
                op::Length => {
                    let top = self.env.stack.top();
                    top.set(fns::length(top.bind(cx))? as i64);
                }
                op::Aref => {
                    let idx = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(data::aref(top.bind(cx), idx.try_into()?, cx)?);
                }
                op::Aset => {
                    let newlet = self.env.stack.pop(cx);
                    let idx = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(data::aset(top.bind(cx), idx.try_into()?, newlet)?);
                }
                op::SymbolValue => {
                    let top = self.env.stack.top().bind_as(cx)?;
                    let value = data::symbol_value(top, self.env, cx).unwrap_or_default();
                    self.env.stack.top().set(value);
                }
                op::SymbolFunction => {
                    let top = self.env.stack.top();
                    top.set(data::symbol_function(top.bind_as(cx)?, cx));
                }
                op::Set => {
                    let newlet = self.env.stack.pop(cx);
                    let top = self.env.stack.top().bind_as(cx)?;
                    let value = data::set(top, newlet, self.env)?;
                    self.env.stack.top().set(value);
                }
                op::Fset => {
                    let def = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set::<Object>(data::fset(top.bind_as(cx)?, def)?.into());
                }
                op::Get => {
                    let prop = self.env.stack.pop(cx).try_into()?;
                    let top = self.env.stack.top().bind_as(cx)?;
                    let value = data::get(top, prop, self.env, cx);
                    self.env.stack.top().set(value);
                }
                op::Substring => return Err(self.unsupported_opcode(op, cx)),
                op::Concat2 => self.concatenate(2, cx)?,
                op::Concat3 => self.concatenate(3, cx)?,
                op::Concat4 => self.concatenate(4, cx)?,
                op::Sub1 => {
                    let top = self.env.stack.top();
                    top.set(cx.add(arith::sub_one(top.bind_as(cx)?)));
                }
                op::Add1 => {
                    let top = self.env.stack.top();
                    top.set(cx.add(arith::add_one(top.bind_as(cx)?)));
                }
                op::EqlSign => {
                    let rhs = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set::<Object>(arith::num_eq(top.bind_as(cx)?, &[rhs.try_into()?]).into());
                }
                op::GreaterThan => {
                    let v1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(arith::greater_than(top.bind_as(cx)?, &[v1.try_into()?]));
                }
                op::LessThan => {
                    let v1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(arith::less_than(top.bind_as(cx)?, &[v1.try_into()?]));
                }
                op::LessThanOrEqual => {
                    let v1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(arith::less_than_or_eq(top.bind_as(cx)?, &[v1.try_into()?]));
                }
                op::GreaterThanOrEqual => {
                    let v1 = &[self.env.stack.pop(cx).try_into()?];
                    let top = self.env.stack.top();
                    top.set(arith::greater_than_or_eq(top.bind_as(cx)?, v1));
                }
                op::Diff => {
                    let right = self.env.stack[0].bind_as(cx)?;
                    let left = self.env.stack[1].bind_as(cx)?;
                    let result = arith::sub(Some(left), &[right]);
                    self.env.stack.pop(cx);
                    self.env.stack.top().set(cx.add(result));
                }
                op::Negate => {
                    let top = self.env.stack.top();
                    top.set(cx.add(arith::sub(top.bind_as(cx)?, &[])));
                }
                op::Plus => {
                    let arg1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    let args = &[top.bind_as(cx)?, arg1.try_into()?];
                    top.set(cx.add(arith::add(args)));
                }
                op::Max => {
                    let arg1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    let args = &[arg1.try_into()?];
                    top.set(cx.add(arith::max(top.bind_as(cx)?, args)));
                }
                op::Min => {
                    let arg1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    let args = &[arg1.try_into()?];
                    top.set(cx.add(arith::min(top.bind_as(cx)?, args)));
                }
                op::Multiply => {
                    let arg1 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    let args = &[top.bind_as(cx)?, arg1.try_into()?];
                    top.set(cx.add(arith::mul(args)));
                }
                op::Point => return Err(self.unsupported_opcode(op, cx)),
                op::GotoChar => return Err(self.unsupported_opcode(op, cx)),
                op::Insert => return Err(self.unsupported_opcode(op, cx)),
                op::PointMax => return Err(self.unsupported_opcode(op, cx)),
                op::PointMin => return Err(self.unsupported_opcode(op, cx)),
                op::CharAfter => return Err(self.unsupported_opcode(op, cx)),
                op::FollowingChar => return Err(self.unsupported_opcode(op, cx)),
                op::PrecedingChar => return Err(self.unsupported_opcode(op, cx)),
                op::CurrentColumn => return Err(self.unsupported_opcode(op, cx)),
                op::IndentTo => return Err(self.unsupported_opcode(op, cx)),
                op::EndOfLineP => return Err(self.unsupported_opcode(op, cx)),
                op::EndOfBufferP => return Err(self.unsupported_opcode(op, cx)),
                op::BeginningOfLineP => return Err(self.unsupported_opcode(op, cx)),
                op::BeginningOfBufferP => return Err(self.unsupported_opcode(op, cx)),
                op::CurrentBuffer => return Err(self.unsupported_opcode(op, cx)),
                op::SetBuffer => return Err(self.unsupported_opcode(op, cx)),
                op::SaveCurrentBuffer1 => return Err(self.unsupported_opcode(op, cx)),
                op::ForwardChar => return Err(self.unsupported_opcode(op, cx)),
                op::ForwardWord => return Err(self.unsupported_opcode(op, cx)),
                op::SkipCharsForward => return Err(self.unsupported_opcode(op, cx)),
                op::SkipCharsBackward => return Err(self.unsupported_opcode(op, cx)),
                op::ForwardLine => return Err(self.unsupported_opcode(op, cx)),
                op::CharSyntax => return Err(self.unsupported_opcode(op, cx)),
                op::BufferSubstring => return Err(self.unsupported_opcode(op, cx)),
                op::DeleteRegion => return Err(self.unsupported_opcode(op, cx)),
                op::NarrowToRegion => return Err(self.unsupported_opcode(op, cx)),
                op::Widen => return Err(self.unsupported_opcode(op, cx)),
                op::EndOfLine => return Err(self.unsupported_opcode(op, cx)),
                op::ConstantN2 => {
                    let idx = instruction.operand().map_err(anyhow::Error::from)?;
                    let cnst = self.get_const(idx.into(), cx)?;
                    self.env.stack.push(cnst);
                }
                op::Goto => {
                    let offset = instruction.operand().map_err(anyhow::Error::from)?;
                    self.pc.goto(usize::from(offset)).map_err(anyhow::Error::from)?;
                }
                op::GotoIfNil => {
                    let offset = instruction.operand().map_err(anyhow::Error::from)?;
                    self.require_stack(1)?;
                    let cond = self.env.stack.pop(cx);
                    if cond.is_nil() {
                        self.pc.goto(usize::from(offset)).map_err(anyhow::Error::from)?;
                    }
                }
                op::GotoIfNonNil => {
                    let offset = instruction.operand().map_err(anyhow::Error::from)?;
                    self.require_stack(1)?;
                    let cond = self.env.stack.pop(cx);
                    if !cond.is_nil() {
                        self.pc.goto(usize::from(offset)).map_err(anyhow::Error::from)?;
                    }
                }
                op::GotoIfNilElsePop => {
                    let offset = instruction.operand().map_err(anyhow::Error::from)?;
                    self.require_stack(1)?;
                    if self.env.stack[0].bind(cx).is_nil() {
                        self.pc.goto(usize::from(offset)).map_err(anyhow::Error::from)?;
                    } else {
                        self.env.stack.pop(cx);
                    }
                }
                op::GotoIfNonNilElsePop => {
                    let offset = instruction.operand().map_err(anyhow::Error::from)?;
                    self.require_stack(1)?;
                    if self.env.stack[0].bind(cx).is_nil() {
                        self.env.stack.pop(cx);
                    } else {
                        self.pc.goto(usize::from(offset)).map_err(anyhow::Error::from)?;
                    }
                }
                op::Return => {
                    self.require_stack(1)?;
                    let checkpoint = *self.binding_frames.last().expect("VM has a binding frame");
                    self.env.unwind_bindings(checkpoint.binding_depth, cx)?;
                    self.env.catch_stack.truncate(checkpoint.catch_depth);
                    // Malformed bytecode may return without PopHandler. Such
                    // handlers must not outlive their frame and intercept a
                    // later caller error; the caller's lower-depth handlers stay.
                    let frame = self.env.stack.current_frame();
                    self.handlers.bind_mut(cx).retain(|handler| handler.stack_frame < frame);
                    if let Some((f, offset)) = self.env.stack.prev_bytecode_frame() {
                        self.binding_frames.pop();
                        self.set_current_frame(f.bind(cx), offset)?;
                        let top = self.env.stack.top().bind(cx);
                        self.env.stack.pop_frame();
                        self.env.stack.push(top);
                    } else {
                        let top = self.env.stack.pop(cx);
                        return Ok(top);
                    }
                }
                op::Discard => {
                    if self.env.stack.frame_len() == 0 {
                        bail_err!("Bytecode stack discard out of range");
                    }
                    self.env.stack.pop(cx);
                }
                op::DiscardN => {
                    let arg = instruction.operand().map_err(anyhow::Error::from)?;
                    let cur_len = self.env.stack.len();
                    let keep_tos = (arg & 0x80) != 0;
                    let count = (arg & 0x7F) as usize;
                    let available = self.env.stack.frame_len();
                    if count > available || (keep_tos && count == available) {
                        bail_err!("Bytecode stack discard out of range");
                    }
                    if keep_tos {
                        let top = self.env.stack.top().bind(cx);
                        self.env.stack.truncate(cur_len - count);
                        self.env.stack.top().set(top);
                    } else {
                        self.env.stack.truncate(cur_len - count);
                    }
                }
                op::Duplicate => self.stack_ref(0, cx)?,
                op::SaveExcursion => return Err(self.unsupported_opcode(op, cx)),
                op::SaveRestriction => return Err(self.unsupported_opcode(op, cx)),
                op::UnwindProtect => return Err(self.unsupported_opcode(op, cx)),
                op::SetMarker => return Err(self.unsupported_opcode(op, cx)),
                op::MatchBeginning => return Err(self.unsupported_opcode(op, cx)),
                op::MatchEnd => return Err(self.unsupported_opcode(op, cx)),
                op::Upcase => return Err(self.unsupported_opcode(op, cx)),
                op::Downcase => return Err(self.unsupported_opcode(op, cx)),
                op::StringEqlSign => return Err(self.unsupported_opcode(op, cx)),
                op::StringLessThan => return Err(self.unsupported_opcode(op, cx)),
                op::Equal => {
                    let rhs = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::equal(top.bind(cx), rhs));
                }
                op::Nthcdr => {
                    let list = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::nthcdr(top.bind_as(cx)?, list.try_into()?)?.as_obj_copy());
                }
                op::Elt => {
                    let n = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::elt(top.bind(cx), n.try_into()?, cx)?);
                }
                op::Member => {
                    let list = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::member(top.bind(cx), list.try_into()?)?);
                }
                op::Assq => {
                    let alist = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::assq(top.bind(cx), alist.try_into()?)?);
                }
                op::Nreverse => {
                    let elt = self.env.stack.top();
                    elt.set(fns::nreverse(elt.bind_as(cx)?)?);
                }
                op::Setcar => {
                    let newcar = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(data::setcar(top.bind_as(cx)?, newcar)?);
                }
                op::Setcdr => {
                    let newcdr = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(data::setcdr(top.bind_as(cx)?, newcdr)?);
                }
                op::CarSafe => {
                    let top = self.env.stack.top();
                    top.set(data::car_safe(top.bind(cx)));
                }
                op::CdrSafe => {
                    let top = self.env.stack.top();
                    top.set(data::cdr_safe(top.bind(cx)));
                }
                op::Nconc => {
                    let list2 = self.env.stack.pop(cx);
                    let top = self.env.stack.top();
                    top.set(fns::nconc(&[top.bind_as(cx)?, list2.try_into()?])?);
                }
                op::Quo | op::Rem => {
                    let right = self.env.stack[0].bind_as(cx)?;
                    let left = self.env.stack[1].bind_as(cx)?;
                    let result = if op == op::Quo {
                        arith::div(left, &[right])?
                    } else {
                        arith::remainder(left, right)?
                    };
                    self.env.stack.pop(cx);
                    self.env.stack.top().set(cx.add(result));
                }
                op::Numberp => {
                    let top = self.env.stack.top();
                    top.set(data::numberp(top.bind(cx)));
                }
                op::Integerp => {
                    let top = self.env.stack.top();
                    top.set(data::integerp(top.bind(cx)));
                }
                op::ListN => {
                    let size = instruction.operand().map_err(anyhow::Error::from)? as usize;
                    if size > self.env.stack.frame_len() {
                        bail_err!("Bytecode list count out of range");
                    }
                    if size == 0 {
                        if !self.env.stack.can_push() {
                            bail_err!("Bytecode stack depth exceeded");
                        }
                        self.env.stack.push(NIL);
                    } else {
                        let slice = Rt::bind_slice(&self.env.stack[..size], cx);
                        let list = alloc::list(slice, cx);
                        let len = self.env.stack.len();
                        self.env.stack.truncate(len - (size - 1));
                        self.env.stack.top().set(list);
                    }
                }
                op::ConcatN => self.concatenate(
                    usize::from(instruction.operand().map_err(anyhow::Error::from)?),
                    cx,
                )?,
                op::InsertN => return Err(self.unsupported_opcode(op, cx)),
                op::Switch => {
                    self.require_stack(2)?;
                    // Validate both operands and the target before consuming
                    // either stack value. General stack-depth validation is
                    // separate from checking switch operand types here.
                    let table = self.env.stack[0].bind(cx);
                    let ObjectType::HashTable(table) = table.untag() else {
                        bail_err!("Invalid bytecode switch table: expected hash table")
                    };
                    let cond = self.env.stack[1].bind(cx);
                    if let Some(offset) = table.get(cond) {
                        let ObjectType::Int(offset) = offset.untag() else {
                            bail_err!("Invalid bytecode switch target: expected integer")
                        };
                        let offset = usize::try_from(offset).map_err(|_| {
                            anyhow::anyhow!(
                                "Invalid bytecode switch target: negative or unrepresentable"
                            )
                        })?;
                        self.pc.goto(offset).map_err(anyhow::Error::from)?;
                    }
                    self.env.stack.pop(cx);
                    self.env.stack.pop(cx);
                }
                op::Constant0
                | op::Constant1
                | op::Constant2
                | op::Constant3
                | op::Constant4
                | op::Constant5
                | op::Constant6
                | op::Constant7
                | op::Constant8
                | op::Constant9
                | op::Constant10
                | op::Constant11
                | op::Constant12
                | op::Constant13
                | op::Constant14
                | op::Constant15
                | op::Constant16
                | op::Constant17
                | op::Constant18
                | op::Constant19
                | op::Constant20
                | op::Constant21
                | op::Constant22
                | op::Constant23
                | op::Constant24
                | op::Constant25
                | op::Constant26
                | op::Constant27
                | op::Constant28
                | op::Constant29
                | op::Constant30
                | op::Constant31
                | op::Constant32
                | op::Constant33
                | op::Constant34
                | op::Constant35
                | op::Constant36
                | op::Constant37
                | op::Constant38
                | op::Constant39
                | op::Constant40
                | op::Constant41
                | op::Constant42
                | op::Constant43
                | op::Constant44
                | op::Constant45
                | op::Constant46
                | op::Constant47
                | op::Constant48
                | op::Constant49
                | op::Constant50
                | op::Constant51
                | op::Constant52
                | op::Constant53
                | op::Constant54
                | op::Constant55
                | op::Constant56
                | op::Constant57
                | op::Constant58
                | op::Constant59
                | op::Constant60
                | op::Constant61
                | op::Constant62
                | op::Constant63 => {
                    let idx = (op as u8) - (op::Constant0 as u8);
                    let cnst = self.get_const(idx as usize, cx)?;
                    self.env.stack.push(cnst);
                }
            }
        }
    }
}

#[defun]
fn byte_code<'ob>(
    bytestr: &Rto<Gc<&ByteString>>,
    vector: &Rto<Gc<&LispVec>>,
    maxdepth: usize,
    env: &mut Rt<Env>,
    cx: &'ob mut Context,
) -> Result<Object<'ob>> {
    let fun = crate::alloc::make_byte_code(
        0,
        bytestr.untag(cx),
        vector.untag(cx),
        maxdepth,
        None,
        None,
        &[],
        cx,
    )?;
    root!(fun, cx);
    Ok(call(fun, 0, "unnamed", &mut CallFrame::new(env), cx)?)
}

/// Structural verifier for future .elc loading and compiler diagnostics.
/// This checks instruction encoding and static target boundaries only, not
/// operand types, stack joins, dynamic switch tables or opcode availability.
pub(crate) fn validate_bytecode(function: &ByteFn) -> Result<()> {
    decoder::Program::decode(function.codes()).validate()?;
    Ok(())
}

/// Number of arguments needed to fill out the remaining slots on the stack.
/// If a function has 3 required args and 2 optional, and it is called with
/// 4 arguments, then 1 will be returned. Indicating that 1 additional `nil`
/// argument should be added to the stack.
fn num_of_fill_args(spec: FnArgs, args: u16, name: &str, cx: &Context) -> Result<u16> {
    if args < spec.required {
        bail!(LispError::arg_cnt(name, spec.required, args, cx));
    }
    let total = spec.required + spec.optional;
    if !spec.rest && (args > total) {
        bail!(LispError::arg_cnt(name, total, args, cx));
    }
    Ok(total.saturating_sub(args))
}

#[defun]
fn fetch_bytecode(_object: Object) {
    // TODO: Implement
}

pub(crate) fn call<'ob>(
    func: &Rto<&ByteFn>,
    arg_cnt: usize,
    name: &str,
    frame: &mut CallFrame,
    cx: &'ob mut Context,
) -> EvalResult<'ob> {
    frame.stack.set_depth(func.bind(cx).depth, cx)?;
    let func = func.bind(cx);
    let mut programs = ProgramCache::default();
    let pc = programs.cursor(func.codes(), 0).map_err(anyhow::Error::from)?;
    let binding_depth = frame.binding_depth();
    let catch_depth = frame.catch_stack.len();
    let entry_frame = frame.stack.current_frame();
    let vm = VM {
        pc,
        programs,
        binding_frames: vec![FrameCheckpoint { binding_depth, catch_depth }],
        func: Slot::new(func),
        env: frame,
        handlers: Vec::new(),
    };
    root!(vm, cx);
    vm.prepare_lisp_args(func, arg_cnt, name, cx)?;
    let result = rebind!(vm.run(cx)).map_err(|e| e.add_trace(name, vm.env.stack.current_args()));
    // Uncaught failures can leave several optimized bytecode frames active.
    // The external CallFrame guard only owns the entry frame: remove nested
    // frames (and their operands) before its eventual single pop on Drop.
    while vm.env.stack.current_frame() > entry_frame {
        vm.env.stack.pop_frame();
    }
    vm.env.catch_stack.truncate(catch_depth);
    vm.env.unwind_bindings(binding_depth, cx)?;
    result
}

#[cfg(test)]
mod test {
    use crate::core::{
        gc::RootSet,
        object::{HashTable, IntoObject},
    };
    use rune_core::macros::{list, rebind, root};

    use super::{opcode::OpCode, *};

    macro_rules! make_bytecode { (
        $name:ident,
        $arglist:expr,
        [$($opcodes:expr),* $(,)?],
        [$($constants:expr),* $(,)?],
        $cx:expr $(,)?
    ) => (
        // https://github.com/rust-lang/rust-analyzer/issues/11681
        let cx1: &Context = $cx;
        let constants: &LispVec = {
            let vec: Vec<Object> = vec![$(cx1.add($constants)),*];
            vec.into_obj(cx1).untag()
        };
        let opcodes = {
            #[allow(trivial_numeric_casts)]
            let opcodes = vec![$($opcodes as u8),*];
            println!("Test seq: {opcodes:?}");
            opcodes.into_obj(cx1).untag()
        };
        // TODO: we should probably caculate the actual depth
        let depth = 10;
        let bytecode = crate::alloc::make_byte_code(
            $arglist,
            &opcodes,
            constants,
            depth,
            None,
            None,
            &[],
            cx1
        ).unwrap();
        root!(bytecode, cx1);
        let $name = bytecode;
        )
    }

    macro_rules! check_bytecode { (
        $bytecode:expr,
        [$($args:expr),* $(,)?],
        $expect:expr,
        $cx:expr $(,)?
    ) => ({
            let bytecode: &Rto<&ByteFn> = $bytecode;
            let cx: &mut Context = $cx;

            let args: Vec<Object> = { vec![$(cx.add($args)),*] };
            let expect = cx.add($expect);

            root!(args, cx);
            root!(expect, cx);

            check_bytecode_internal(
                args,
                bytecode,
                expect,
                cx
            );
        })
    }

    fn check_bytecode_internal(
        args: &mut Rt<Vec<Slot<Object>>>,
        bytecode: &Rto<&ByteFn>,
        expect: &Rto<Object>,
        cx: &mut Context,
    ) {
        root!(env, new(Env), cx);
        let frame = &mut CallFrame::new(env);
        frame.push_arg_slice(Rt::bind_slice(args, cx));
        frame.finalize_arguments();
        let val = rebind!(call(bytecode, frame.arg_count(), "test", frame, cx).unwrap());
        let expect = expect.bind(cx);
        assert_eq!(val, expect);
    }

    #[test]
    fn pushcatch_handles_native_call2_and_preserves_external_catches() {
        // GNU byte-compile of (lambda () (catch 'tag (throw 'tag 7))).
        // Replace the tag constant by self-evaluating 17; actual depth is 3.
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let constants =
            vec![Object::from(17), sym::THROW.into(), Object::from(7)].into_obj(&cx.block);
        let codes = vec![192u8, 50, 9, 0, 193, 192, 194, 34, 48, 135].into_obj(&cx.block);
        let bytecode = crate::alloc::make_byte_code(
            0,
            &codes.untag(),
            constants.untag(),
            3,
            None,
            None,
            &[],
            cx,
        )
        .unwrap();
        root!(bytecode, cx);
        root!(env, new(Env), cx);
        env.catch_stack.push(Object::from(29));
        for _ in 0..3 {
            let mut frame = CallFrame::new(env);
            frame.finalize_arguments();
            assert_eq!(call(bytecode, 0, "native-catch", &mut frame, cx).unwrap(), Object::from(7));
            assert_eq!(frame.catch_stack.len(), 1);
            assert_eq!(frame.catch_stack[0].bind(cx), Object::from(29));
        }
    }

    #[test]
    fn pushcatch_popped_or_returned_handlers_do_not_leak_tags() {
        use OpCode as O;
        for pop in [false, true] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            let codes = if pop {
                vec![
                    O::Constant0 as u8,
                    O::PushCatch as u8,
                    7,
                    0,
                    O::PopHandler as u8,
                    O::Constant1 as u8,
                    O::Return as u8,
                    O::Return as u8,
                ]
            } else {
                vec![
                    O::Constant0 as u8,
                    O::PushCatch as u8,
                    6,
                    0,
                    O::Constant1 as u8,
                    O::Return as u8,
                    O::Return as u8,
                ]
            };
            let codes = codes.into_obj(&cx.block);
            let constants = vec![Object::from(17), Object::from(7)].into_obj(&cx.block);
            let bytecode = crate::alloc::make_byte_code(
                0,
                &codes.untag(),
                constants.untag(),
                1,
                None,
                None,
                &[],
                cx,
            )
            .unwrap();
            root!(bytecode, cx);
            root!(env, new(Env), cx);
            env.catch_stack.push(Object::from(29));
            let mut frame = CallFrame::new(env);
            frame.finalize_arguments();
            assert_eq!(call(bytecode, 0, "retire-catch", &mut frame, cx).unwrap(), Object::from(7));
            assert_eq!(frame.catch_stack.len(), 1);
            assert_eq!(frame.catch_stack[0].bind(cx), Object::from(29));
        }
    }

    #[test]
    fn pushcatch_skips_mismatched_tag_and_condition_handler() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        // Outer catch17; inner catch19; condition-case ERROR must skip throw.
        make_bytecode!(
            bytecode,
            0,
            [
                O::Constant0,
                O::PushCatch,
                19,
                0,
                O::Constant1,
                O::PushCatch,
                18,
                0,
                O::Constant2,
                O::PushCondtionCase,
                18,
                0,
                O::Constant3,
                O::Constant0,
                O::Constant4,
                O::Call2,
                O::PopHandler,
                O::PopHandler,
                O::Return,
                O::Return
            ],
            [17, 19, sym::ERROR, sym::THROW, 7],
            cx
        );
        check_bytecode!(bytecode, [], 7, cx);
    }

    #[test]
    fn pushcatch_does_not_intercept_errors_and_uncaught_exit_cleans_tags() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        make_bytecode!(
            bytecode,
            0,
            [O::Constant0, O::PushCatch, 5, 0, O::Car, O::Return],
            [17],
            cx
        );
        root!(env, new(Env), cx);
        for _ in 0..3 {
            let mut frame = CallFrame::new(env);
            frame.finalize_arguments();
            assert!(call(bytecode, 0, "catch-error", &mut frame, cx).is_err());
            assert!(frame.catch_stack.is_empty());
        }
    }

    #[test]
    fn pushcatch_unwinds_optimized_callee_and_dynamic_bindings() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let variable = crate::core::env::intern("pushcatch-nested-variable", cx);
        make_bytecode!(
            inner,
            0,
            [
                O::Constant1,
                O::VarBind0,
                O::Constant2,
                O::Constant3,
                O::Constant1,
                O::Call2,
                O::Return
            ],
            [variable, 7, sym::THROW, 17],
            cx
        );
        root!(inner_obj, cx.add(inner.bind(cx)), cx);
        make_bytecode!(
            outer,
            0,
            [
                O::Constant0,
                O::PushCatch,
                7,
                0,
                O::Constant1,
                O::Call0,
                O::Return,
                O::Discard,
                O::VarRef2,
                O::Return
            ],
            [17, inner_obj.bind(cx), variable],
            cx
        );
        root!(env, new(Env), cx);
        let variable = crate::core::env::intern("pushcatch-nested-variable", cx);
        env.varbind(variable, Object::from(3), cx);
        let initial = env.binding_depth();
        for _ in 0..3 {
            let mut frame = CallFrame::new(env);
            frame.finalize_arguments();
            assert_eq!(call(outer, 0, "nested-catch", &mut frame, cx).unwrap(), Object::from(3));
            assert_eq!(frame.binding_depth(), initial);
            assert!(frame.catch_stack.is_empty());
        }
    }

    #[test]
    fn pushcatch_allocated_tag_is_rooted_across_interpreted_collection() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let tag = Cons::new1(17, cx);
        root!(tag, cx);
        let form =
            crate::reader::read("(function (lambda (tag) (garbage-collect) (throw tag 7)))", cx)
                .unwrap()
                .0;
        root!(form, cx);
        root!(env, new(Env), cx);
        let closure = crate::interpreter::eval(form, None, env, cx).unwrap();
        root!(closure, cx);
        make_bytecode!(
            bytecode,
            0,
            [
                O::Constant0,
                O::PushCatch,
                9,
                0,
                O::Constant1,
                O::Constant0,
                O::Call1,
                O::PopHandler,
                O::Return,
                O::Return
            ],
            [tag.bind(cx), closure.bind(cx)],
            cx
        );
        let mut frame = CallFrame::new(env);
        frame.finalize_arguments();
        assert_eq!(call(bytecode, 0, "allocated-catch", &mut frame, cx).unwrap(), Object::from(7));
        assert!(frame.catch_stack.is_empty());
    }

    #[test]
    fn pushcatch_rejects_operand_target_before_registering_tag() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        make_bytecode!(bytecode, 0, [O::Constant0, O::PushCatch, 2, 0, O::Return], [17], cx);
        root!(env, new(Env), cx);
        let mut frame = CallFrame::new(env);
        frame.finalize_arguments();
        assert!(call(bytecode, 0, "bad-catch-target", &mut frame, cx).is_err());
        assert!(frame.catch_stack.is_empty());
    }

    #[test]
    fn m0_control_opcodes_reject_missing_operands_without_using_caller_values() {
        use OpCode as O;
        for opcode in [
            O::Call0,
            O::Call5,
            O::CallN,
            O::CallN2,
            O::Return,
            O::GotoIfNil,
            O::GotoIfNonNil,
            O::GotoIfNilElsePop,
            O::GotoIfNonNilElsePop,
            O::PushCondtionCase,
            O::PushCatch,
            O::Switch,
            O::VarSet0,
            O::VarBind0,
        ] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            make_bytecode!(bytecode, 0, [opcode, 0, 0], [sym::FILL_COLUMN], cx);
            root!(env, new(Env), cx);
            env.stack.push(Object::from(99_i64));
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let before = frame.stack.len();
            let error = call(bytecode, 0, "missing-control-operands", frame, cx).unwrap_err();
            assert!(error.to_string().contains("Bytecode operand stack underflow"));
            assert_eq!(frame.stack.len(), before);
        }
    }

    #[test]
    fn m0_calls_validate_argument_count_before_reading_target() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        make_bytecode!(missing_argument, 0, [O::Constant0, O::Call1], [sym::IDENTITY], cx);
        root!(env, new(Env), cx);
        {
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let before = frame.stack.len();
            assert!(call(missing_argument, 0, "missing-call-argument", frame, cx).is_err());
            assert_eq!(frame.stack.len(), before + 1);
        }
        make_bytecode!(
            valid_call,
            0,
            [O::Constant0, O::Constant1, O::Call1, O::Return],
            [sym::IDENTITY, 7],
            cx
        );
        check_bytecode!(valid_call, [], 7, cx);
    }

    #[test]
    fn m0_stack_operand_checks_do_not_cross_frames_or_mutate_on_error() {
        use OpCode as O;
        for (opcode, operand) in [
            (O::StackRef0, 0),
            (O::StackRefN, 0),
            (O::StackRefN2, 0),
            (O::StackSetN, 0),
            (O::StackSetN2, 0),
            (O::Duplicate, 0),
            (O::Discard, 0),
            (O::DiscardN, 1),
            (O::DiscardN, 0x80),
            (O::ListN, 1),
        ] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            make_bytecode!(bytecode, 0, [opcode, operand, 0], [], cx);
            root!(env, new(Env), cx);
            // A caller value is present, but inaccessible to the empty callee.
            env.stack.push(Object::from(99_i64));
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let before = frame.stack.len();
            assert!(call(bytecode, 0, "invalid-stack-operand", frame, cx).is_err());
            assert_eq!(frame.stack.len(), before);
        }
    }

    fn depth_fixture<'ob>(depth: usize, cx: &'ob Context) -> &'ob ByteFn {
        let constants: &LispVec = vec![cx.add(7)].into_obj(cx).untag();
        let codes: &ByteString =
            vec![OpCode::Constant0 as u8, OpCode::Return as u8].into_obj(cx).untag();
        crate::alloc::make_byte_code(0, codes, constants, depth, None, None, &[], cx).unwrap()
    }

    #[test]
    fn large_declared_depth_reserves_without_arbitrary_initial_or_nested_caps() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let inner = depth_fixture(4096, cx);
        root!(inner, cx);
        check_bytecode!(inner, [], 7, cx);
        let target = cx.add(inner.bind(cx));
        root!(target, cx);
        make_bytecode!(
            outer,
            0,
            [O::Constant0, O::Constant1, O::Call0, O::Plus, O::Return],
            [5, target.bind(cx)],
            cx
        );
        check_bytecode!(outer, [], 12, cx);
    }

    #[test]
    fn impossible_initial_depth_returns_error_without_mutating_stack() {
        for depth in [usize::MAX, usize::MAX / 2] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            let function = depth_fixture(depth, cx);
            root!(function, cx);
            root!(env, new(Env), cx);
            env.stack.push(Object::from(99));
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let before = frame.stack.len();
            assert!(call(function, 0, "impossible-depth", frame, cx).is_err());
            assert_eq!(frame.stack.len(), before);
        }
    }

    #[test]
    fn nested_reservation_failure_preserves_callers_frame_and_handler() {
        use OpCode as O;
        for depth in [usize::MAX, usize::MAX / 2] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            let bad = depth_fixture(depth, cx);
            root!(bad, cx);
            let condition = Cons::new1(sym::ERROR, cx);
            // Keep a prefix so usize::MAX overflows the absolute frame end.
            make_bytecode!(
                outer,
                0,
                [
                    O::Constant2,
                    O::Constant0,
                    O::PushCondtionCase,
                    8,
                    0,
                    O::Constant1,
                    O::Call0,
                    O::Return,
                    O::Discard,
                    O::Return
                ],
                [condition, bad.bind(cx), 42],
                cx
            );
            check_bytecode!(outer, [], 42, cx);
        }
    }

    #[test]
    fn argument_preparation_checks_depth_before_optional_or_rest_pushes() {
        use OpCode as O;
        // Both specs need eleven parameter slots but this fixture declares ten.
        // Optional filling and a missing rest list must signal, not stack panic.
        for spec in [11 << 8, (10 << 8) | 10 | 0x80] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            make_bytecode!(bytecode, spec, [O::Constant0, O::Return], [7], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            let argc = if spec & 0x80 != 0 { 10 } else { 0 };
            for _ in 0..argc {
                frame.push_arg(Object::from(3));
            }
            frame.finalize_arguments();
            let before = frame.stack.len();
            let error = call(bytecode, argc, "argument-depth", frame, cx).unwrap_err();
            assert!(error.to_string().contains("stack depth exceeded by arguments"));
            assert_eq!(frame.stack.len(), before);
        }
    }

    #[test]
    fn m0_stack_growth_checks_declared_depth_before_mutation() {
        use OpCode as O;
        for opcode in [O::StackRef0, O::Duplicate, O::ListN] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            // Ten required arguments already fill make_bytecode!'s depth10.
            make_bytecode!(bytecode, 2570, [opcode, 0], [], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.push_arg_slice(&[Object::from(1_i64); 10]);
            frame.finalize_arguments();
            let before = frame.stack.len();
            let error = call(bytecode, 10, "full-stack", frame, cx).unwrap_err();
            assert!(error.to_string().contains("Bytecode stack depth exceeded"));
            assert_eq!(frame.stack.len(), before);
        }
    }

    #[test]
    fn m0_list_zero_and_valid_stack_operations() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        make_bytecode!(empty_list, 0, [O::ListN, 0, O::Return], [], cx);
        check_bytecode!(empty_list, [], NIL, cx);
        make_bytecode!(discard_zero, 0, [O::DiscardN, 0, O::Constant0, O::Return], [7], cx);
        check_bytecode!(discard_zero, [], 7, cx);
        make_bytecode!(
            set_and_ref,
            0,
            [
                O::Constant0,
                O::Constant1,
                O::StackSetN,
                1,
                O::StackRef0,
                O::ListN,
                2,
                O::Return
            ],
            [1, 2],
            cx
        );
        root!(env, new(Env), cx);
        let frame = &mut CallFrame::new(env);
        frame.finalize_arguments();
        let result = call(set_and_ref, 0, "valid-stack-operations", frame, cx).unwrap();
        assert_eq!(result.to_string(), "(2 2)");
    }

    #[test]
    fn m0_truncated_bytecode_and_invalid_jumps_return_errors() {
        use OpCode as O;
        for bytes in [
            vec![],
            vec![O::VarRefN as u8],
            vec![O::ConstantN2 as u8, 0],
            vec![O::Goto as u8, 0],
            vec![O::GotoIfNil as u8],
            vec![O::GotoIfNonNil as u8],
            vec![O::PushCondtionCase as u8],
            vec![O::Goto as u8, 3, 0],
            vec![O::Goto as u8, 0xff, 0xff],
        ] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            let constants: &LispVec = Vec::<Object>::new().into_obj(cx).untag();
            // Use the ordinary byte-code constructor with a complete buffer;
            // only instruction/target validity is intentionally malformed.
            let codes: &ByteString = bytes.into_obj(cx).untag();
            let function =
                crate::alloc::make_byte_code(0, codes, constants, 10, None, None, &[], cx).unwrap();
            root!(function, cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            assert!(call(function, 0, "truncated-bytecode", frame, cx).is_err());
        }
    }

    #[test]
    fn m3_fixed_primitive_opcodes_check_frame_local_stack_before_dispatch() {
        use OpCode as O;
        for opcode in [
            O::Nth,
            O::Symbolp,
            O::Consp,
            O::Stringp,
            O::Listp,
            O::Eq,
            O::Memq,
            O::Not,
            O::Car,
            O::Cdr,
            O::Cons,
            O::List1,
            O::List2,
            O::List3,
            O::List4,
            O::Length,
            O::Aref,
            O::Aset,
            O::SymbolValue,
            O::SymbolFunction,
            O::Set,
            O::Fset,
            O::Get,
            O::Sub1,
            O::Add1,
            O::EqlSign,
            O::GreaterThan,
            O::LessThan,
            O::LessThanOrEqual,
            O::GreaterThanOrEqual,
            O::Diff,
            O::Negate,
            O::Plus,
            O::Max,
            O::Min,
            O::Multiply,
            O::Equal,
            O::Nthcdr,
            O::Elt,
            O::Member,
            O::Assq,
            O::Nreverse,
            O::Setcar,
            O::Setcdr,
            O::CarSafe,
            O::CdrSafe,
            O::Nconc,
            O::Quo,
            O::Rem,
            O::Concat2,
            O::Concat3,
            O::Concat4,
        ] {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            sym::init_symbols();
            make_bytecode!(function, 0, [opcode], [], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let error = call(function, 0, "empty-operands", frame, cx).unwrap_err();
            assert!(error.to_string().contains("operand stack underflow"), "{opcode:?}: {error}");
        }
    }

    #[test]
    fn m3_jump_to_operand_and_invalid_handler_target_signal_errors() {
        use OpCode as O;
        for code in [
            vec![O::Goto as u8, 1, 0],
            vec![O::Constant0 as u8, O::PushCondtionCase as u8, 2, 0],
        ] {
            let roots = RootSet::default();
            let cx = &mut Context::new(&roots);
            sym::init_symbols();
            let constants: &LispVec = vec![cx.add(sym::ERROR)].into_obj(cx).untag();
            let codes: &ByteString = code.into_obj(cx).untag();
            let function =
                crate::alloc::make_byte_code(0, codes, constants, 10, None, None, &[], cx).unwrap();
            root!(function, cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            assert!(
                call(function, 0, "operand-target", frame, cx)
                    .unwrap_err()
                    .to_string()
                    .contains("instruction boundary")
            );
        }
    }

    #[test]
    fn m3_arithmetic_opcodes_share_checked_primitive_semantics() {
        use OpCode as O;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        make_bytecode!(
            difference,
            0,
            [O::Constant0, O::Constant1, O::Diff, O::Return],
            [11, 4],
            cx
        );
        check_bytecode!(difference, [], 7, cx);
        make_bytecode!(quotient, 0, [O::Constant0, O::Constant1, O::Quo, O::Return], [-11, 4], cx);
        check_bytecode!(quotient, [], -2, cx);
        make_bytecode!(remainder, 0, [O::Constant0, O::Constant1, O::Rem, O::Return], [-11, 4], cx);
        check_bytecode!(remainder, [], -3, cx);
        make_bytecode!(floating, 0, [O::Constant0, O::Constant1, O::Quo, O::Return], [11, 4.0], cx);
        check_bytecode!(floating, [], 2.75, cx);
        for opcode in [O::Quo, O::Rem] {
            make_bytecode!(zero, 0, [O::Constant0, O::Constant1, opcode, O::Return], [1, 0], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            assert!(
                call(zero, 0, "zero-divisor", frame, cx)
                    .unwrap_err()
                    .to_string()
                    .contains("arith-error")
            );
        }
    }

    #[test]
    fn m3_concat_opcodes_preserve_argument_order_and_unicode_characters() {
        use OpCode as O;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        make_bytecode!(
            two,
            0,
            [O::Constant0, O::Constant1, O::Concat2, O::Return],
            ["a", "\u{e9}"],
            cx
        );
        check_bytecode!(two, [], "a\u{e9}", cx);
        make_bytecode!(
            three,
            0,
            [O::Constant0, O::Constant1, O::Constant2, O::Concat3, O::Return],
            ["a", NIL, "b"],
            cx
        );
        check_bytecode!(three, [], "ab", cx);
        make_bytecode!(
            four,
            0,
            [O::Constant0, O::Constant1, O::Constant2, O::Constant3, O::Concat4, O::Return],
            ["a", "b", "c", "d"],
            cx
        );
        check_bytecode!(four, [], "abcd", cx);
        make_bytecode!(
            many,
            0,
            [O::Constant0, O::Constant1, O::ConcatN, 2, O::Return],
            ["x", "y"],
            cx
        );
        check_bytecode!(many, [], "xy", cx);
        make_bytecode!(empty, 0, [O::ConcatN, 0, O::Return], [], cx);
        check_bytecode!(empty, [], "", cx);
    }

    #[test]
    fn m3_predecoded_pc_survives_explicit_collection_and_nested_resume() {
        use OpCode as O;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        let collector = crate::core::env::intern("garbage-collect", cx);
        make_bytecode!(
            inner,
            0,
            [O::Constant0, O::Call0, O::Discard, O::Constant1, O::Return],
            [collector, 7],
            cx
        );
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        make_bytecode!(
            outer,
            0,
            [O::Constant0, O::Call0, O::Constant1, O::Plus, O::Return],
            [inner.bind(cx), 5],
            cx
        );
        check_bytecode!(outer, [], 12, cx);
    }

    #[test]
    fn m0_invalid_constant_indices_return_errors_before_stack_access() {
        use OpCode as O;
        for opcode in [O::Constant0, O::VarRef0, O::VarSet0, O::VarBind0, O::ConstantN2] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            // Operand bytes are present, but no constants or stack values are.
            make_bytecode!(bytecode, 0, [opcode, 0xff, 0xff], [], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let error = call(bytecode, 0, "invalid-constant", frame, cx).unwrap_err();
            assert!(error.to_string().contains("Invalid bytecode constant index:"));
        }
    }

    #[test]
    fn m0_variable_opcodes_reject_nonsymbol_constants() {
        use OpCode as O;
        for opcode in [O::VarRef0, O::VarSet0, O::VarBind0] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            // An invalid symbol must signal before VarSet/VarBind pop an empty stack.
            make_bytecode!(bytecode, 0, [opcode, O::Return], [7], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let error = call(bytecode, 0, "nonsymbol-constant", frame, cx).unwrap_err();
            assert!(matches!(error.error, ErrorType::Err(_)));
        }
    }

    #[test]
    fn m0_unknown_opcode_bytes_return_errors() {
        let mut invalid_count = 0;
        for byte in 0_u8..=u8::MAX {
            if OpCode::try_from(byte).is_ok() {
                continue;
            }
            invalid_count += 1;
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            // A complete one-byte stream exercises decoding only. Truncated
            // operands and stack validation are separate, still-open issues.
            make_bytecode!(bytecode, 0, [byte], [], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let error = call(bytecode, 0, "invalid-opcode", frame, cx).unwrap_err();
            assert!(error.to_string().contains("Invalid Bytecode:"), "byte {byte}");
            assert!(matches!(error.error, ErrorType::Err(_)));
        }
        assert!(invalid_count > 0);
    }

    #[test]
    fn dynamic_bindings_restore_after_decoder_error_and_return() {
        use OpCode as O;
        for terminal in [51_u8, O::Return as u8] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            let variable = crate::core::env::intern("vm-binding-cleanup", cx);
            make_bytecode!(
                bytecode,
                0,
                [O::Constant1, O::VarBind0, O::Constant1, terminal],
                [variable, 7],
                cx
            );
            root!(env, new(Env), cx);
            env.varbind(variable, Object::from(3), cx);
            let initial = env.binding_depth();
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let result = call(bytecode, 0, "binding-cleanup", frame, cx);
            assert_eq!(result.is_err(), terminal == 51);
            assert_eq!(frame.binding_depth(), initial);
            assert_eq!(
                frame
                    .vars
                    .get(crate::core::env::intern("vm-binding-cleanup", cx))
                    .unwrap()
                    .bind(cx),
                Object::from(3)
            );
        }
    }

    #[test]
    fn handler_restores_dynamic_binding_checkpoint() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let variable = crate::core::env::intern("vm-handler-binding", cx);
        let condition = Cons::new1(sym::ERROR, cx);
        make_bytecode!(
            bytecode,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                7,
                0,
                O::Constant2,
                O::VarBind1,
                51_u8,
                O::VarRef1,
                O::Return
            ],
            [condition, variable, 7],
            cx
        );
        root!(env, new(Env), cx);
        env.varbind(variable, Object::from(3), cx);
        let initial = env.binding_depth();
        let frame = &mut CallFrame::new(env);
        frame.finalize_arguments();
        let result = call(bytecode, 0, "handler-bindings", frame, cx).unwrap();
        assert_eq!(result, Object::from(3));
        assert_eq!(frame.binding_depth(), initial);
    }

    #[test]
    fn nested_decoder_error_unwinds_bindings_before_outer_handler() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let variable = crate::core::env::intern("vm-nested-handler-binding", cx);
        make_bytecode!(
            inner,
            0,
            [O::Constant1, O::VarBind0, O::Constant1, 51_u8],
            [variable, 7],
            cx
        );
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        let condition = Cons::new1(sym::ERROR, cx);
        make_bytecode!(
            outer,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                8,
                0,
                O::Constant1,
                O::Call0,
                O::PopHandler,
                O::Return,
                O::VarRef2,
                O::Return
            ],
            [condition, inner.bind(cx), variable],
            cx
        );
        root!(env, new(Env), cx);
        env.varbind(variable, Object::from(3), cx);
        let initial = env.binding_depth();
        let frame = &mut CallFrame::new(env);
        frame.finalize_arguments();
        let result = call(outer, 0, "nested-handler-bindings", frame, cx).unwrap();
        assert_eq!(result, Object::from(3));
        assert_eq!(frame.binding_depth(), initial);
    }

    #[test]
    fn malformed_unbind_cannot_remove_callers_dynamic_bindings() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let variable = crate::core::env::intern("vm-unbind-floor", cx);
        make_bytecode!(bytecode, 0, [O::Unbind1], [], cx);
        root!(env, new(Env), cx);
        env.varbind(variable, Object::from(3), cx);
        let initial = env.binding_depth();
        let frame = &mut CallFrame::new(env);
        frame.finalize_arguments();
        assert!(call(bytecode, 0, "binding-floor", frame, cx).is_err());
        assert_eq!(frame.binding_depth(), initial);
        assert_eq!(
            frame
                .vars
                .get(crate::core::env::intern("vm-unbind-floor", cx))
                .unwrap()
                .bind(cx),
            Object::from(3)
        );
    }

    #[test]
    fn malformed_unbind_is_a_lisp_error_not_a_panic() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        make_bytecode!(bytecode, 0, [O::Unbind1], [], cx);
        root!(env, new(Env), cx);
        let frame = &mut CallFrame::new(env);
        frame.finalize_arguments();
        let error = call(bytecode, 0, "binding-underflow", frame, cx).unwrap_err();
        assert!(error.to_string().contains("binding stack underflow"));
    }

    #[test]
    fn condition_handler_does_not_intercept_throw_and_bindings_unwind() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let (closure, _) = crate::reader::read("(closure nil () (throw 17 7))", cx).unwrap();
        root!(closure, cx);
        let variable = crate::core::env::intern("vm-throw-binding", cx);
        let condition = Cons::new1(sym::ERROR, cx);
        make_bytecode!(
            bytecode,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                10,
                0,
                O::Constant3,
                O::VarBind2,
                O::Constant1,
                O::Call0,
                O::PopHandler,
                O::Return,
                O::Constant3,
                O::Return
            ],
            [condition, closure.bind(cx), variable, 7],
            cx
        );
        root!(env, new(Env), cx);
        env.varbind(variable, Object::from(3), cx);
        env.catch_stack.push(Object::from(17));
        let initial = env.binding_depth();
        let frame = &mut CallFrame::new(env);
        frame.finalize_arguments();
        let error = call(bytecode, 0, "throw-through-condition", frame, cx).unwrap_err();
        let ErrorType::Throw(id) = error.error else {
            panic!("condition handler intercepted nonlocal throw");
        };
        let (actual_tag, data) = frame.get_exception(id).unwrap();
        assert_eq!(actual_tag.bind(cx), Object::from(17));
        assert_eq!(data.bind(cx), Object::from(7));
        assert_eq!(frame.binding_depth(), initial);
        assert_eq!(
            frame
                .vars
                .get(crate::core::env::intern("vm-throw-binding", cx))
                .unwrap()
                .bind(cx),
            Object::from(3)
        );
        assert_eq!(frame.catch_stack.len(), 1);
    }

    #[test]
    fn malformed_pop_handler_cannot_remove_callers_handler() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        make_bytecode!(inner, 0, [O::PopHandler, O::Constant0, O::Return], [7], cx);
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        let condition = Cons::new1(sym::ERROR, cx);
        make_bytecode!(
            outer,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                7,
                0,
                O::Constant1,
                O::Call0,
                O::Return,
                O::Constant2,
                O::Return
            ],
            [condition, inner.bind(cx), 42],
            cx
        );
        check_bytecode!(outer, [], 42, cx);
    }

    #[test]
    fn nested_return_drops_callee_handlers_but_preserves_callers() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let condition = Cons::new1(sym::ERROR, cx);
        // Deliberately omit PopHandler in the callee. Its fallback value must
        // never be used after it has returned to its caller.
        make_bytecode!(
            inner,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                6,
                0,
                O::Constant1,
                O::Return,
                O::Constant2,
                O::Return
            ],
            [condition, 7, 99],
            cx
        );
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        make_bytecode!(
            outer,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                8,
                0,
                O::Constant1,
                O::Call0,
                O::Discard,
                51_u8,
                O::Constant2,
                O::Return
            ],
            [condition, inner.bind(cx), 42],
            cx
        );
        check_bytecode!(outer, [], 42, cx);
    }

    #[test]
    fn handler_recovers_from_nested_argument_depth_error_and_calls_again() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        // Eleven optional parameters exceed the fixture's declared depth ten.
        make_bytecode!(bad, 11 << 8, [O::Constant0, O::Return], [7], cx);
        let bad = cx.add(bad.bind(cx));
        root!(bad, cx);
        make_bytecode!(good, 0, [O::Constant0, O::Return], [42], cx);
        let good = cx.add(good.bind(cx));
        root!(good, cx);
        let condition = Cons::new1(sym::ERROR, cx);
        make_bytecode!(
            outer,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                7,
                0,
                O::Constant1,
                O::Call0,
                O::Return,
                O::Discard,
                O::Constant2,
                O::Call0,
                O::Return
            ],
            [condition, bad.bind(cx), good.bind(cx)],
            cx
        );
        // The recovery call checks that both the current bytecode frame and
        // binding-frame checkpoints were restored after failed argument setup.
        check_bytecode!(outer, [], 42, cx);
    }

    #[test]
    fn nested_callee_cannot_consume_its_call_target_as_an_operand() {
        use OpCode as O;
        for opcode in [O::Return, O::StackRef0, O::Duplicate] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            make_bytecode!(inner, 0, [opcode, O::Return], [], cx);
            let inner = cx.add(inner.bind(cx));
            root!(inner, cx);
            let condition = Cons::new1(sym::ERROR, cx);
            make_bytecode!(
                outer,
                0,
                [
                    O::Constant0,
                    O::PushCondtionCase,
                    7,
                    0,
                    O::Constant1,
                    O::Call0,
                    O::Return,
                    O::Constant2,
                    O::Return
                ],
                [condition, inner.bind(cx), 42],
                cx
            );
            check_bytecode!(outer, [], 42, cx);
        }
    }

    #[test]
    fn nested_optional_arguments_use_full_depth_without_hidden_target_slot() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        // Ten nil-filled optionals exactly fill depth ten. The nested target
        // must not take an eleventh slot or be visible beneath the arguments.
        make_bytecode!(inner, 10 << 8, [O::Return], [], cx);
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        make_bytecode!(
            outer,
            0,
            [O::Constant0, O::Constant1, O::Call0, O::Discard, O::Return],
            [7, inner.bind(cx)],
            cx
        );
        check_bytecode!(outer, [], 7, cx);
    }

    #[test]
    fn nested_argument_removal_preserves_order_and_caller_prefix() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        // Three required parameters, maximum three. Returning stack-ref two
        // proves target removal did not swap the last argument into first place.
        make_bytecode!(inner, 771, [O::StackRef2, O::Return], [], cx);
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        make_bytecode!(
            outer,
            0,
            [
                O::Constant0,
                O::Constant1,
                O::Constant2,
                O::Constant3,
                O::Constant4,
                O::Call3,
                O::Plus,
                O::Return
            ],
            [10, inner.bind(cx), 1, 2, 3],
            cx
        );
        check_bytecode!(outer, [], 11, cx);
    }

    #[test]
    fn nested_rest_arguments_exclude_target_and_preserve_unicode_values() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        // No fixed parameters and a rest list. Target removal must leave
        // only the three argument values for rest-list construction.
        make_bytecode!(inner, 128, [O::Return], [], cx);
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        make_bytecode!(
            outer,
            0,
            [O::Constant0, O::Constant1, O::Constant2, O::Constant3, O::Call3, O::Return],
            [inner.bind(cx), "λ", 2, "🙂"],
            cx
        );
        let expected = list!["λ", 2, "🙂"; cx];
        root!(expected, cx);
        check_bytecode!(outer, [], expected, cx);
    }

    #[test]
    fn uncaught_nested_errors_restore_frame_depth_for_repeated_env_reuse() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        make_bytecode!(bad, 0, [51_u8], [], cx);
        let bad_target = cx.add(bad.bind(cx));
        root!(bad_target, cx);
        make_bytecode!(middle, 0, [O::Constant0, O::Call0, O::Return], [bad_target.bind(cx)], cx);
        let middle_target = cx.add(middle.bind(cx));
        root!(middle_target, cx);
        make_bytecode!(outer, 0, [O::Constant0, O::Call0, O::Return], [middle_target.bind(cx)], cx);
        make_bytecode!(good, 0, [O::Constant0, O::Return], [7], cx);
        root!(env, new(Env), cx);
        for _ in 0..4 {
            {
                let frame = &mut CallFrame::new(env);
                frame.finalize_arguments();
                let entry = frame.stack.current_frame();
                let error = call(outer, 0, "uncaught-nested", frame, cx).unwrap_err();
                assert!(error.to_string().contains("Invalid Bytecode:"));
                assert_eq!(frame.stack.current_frame(), entry);
                assert_eq!(frame.stack.frame_len(), 0);
            }
            assert_eq!(env.stack.current_frame(), 0);
            assert_eq!(env.stack.len(), 0);
            {
                let frame = &mut CallFrame::new(env);
                frame.finalize_arguments();
                assert_eq!(call(good, 0, "reuse-after-error", frame, cx).unwrap(), Object::from(7));
            }
            assert_eq!(env.stack.current_frame(), 0);
            assert_eq!(env.stack.len(), 0);
        }
    }

    #[test]
    fn handlers_preserve_structured_wrong_number_of_arguments() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        // A required argument is missing, so entering the nested bytecode
        // function produces LispError rather than a generic anyhow diagnostic.
        make_bytecode!(inner, 257, [O::Constant0, O::Return], [7], cx);
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        let condition = Cons::new1(sym::ERROR, cx);
        make_bytecode!(
            outer,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                7,
                0,
                O::Constant1,
                O::Call0,
                O::Return,
                O::Car,
                O::Return
            ],
            [condition, inner.bind(cx)],
            cx
        );
        check_bytecode!(outer, [], sym::WRONG_NUMBER_OF_ARGUMENTS, cx);
    }

    #[test]
    fn m0_unknown_opcode_uses_existing_error_handler() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let condition = Cons::new1(sym::ERROR, cx);
        assert!(O::try_from(51_u8).is_err());
        make_bytecode!(
            bytecode,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                0x08,
                0x00,
                51_u8,
                O::PopHandler,
                O::Constant1,
                O::Return,
                O::Constant2,
                O::Return
            ],
            [condition, NIL, 7],
            cx
        );
        check_bytecode!(bytecode, [], 7, cx);
    }

    #[test]
    fn m0_unimplemented_opcodes_signal_instead_of_panicking() {
        use OpCode as O;
        for opcode in [
            O::Substring,
            O::Point,
            O::GotoChar,
            O::Insert,
            O::PointMax,
            O::PointMin,
            O::CharAfter,
            O::FollowingChar,
            O::PrecedingChar,
            O::CurrentColumn,
            O::IndentTo,
            O::EndOfLineP,
            O::EndOfBufferP,
            O::BeginningOfLineP,
            O::BeginningOfBufferP,
            O::CurrentBuffer,
            O::SetBuffer,
            O::SaveCurrentBuffer1,
            O::ForwardChar,
            O::ForwardWord,
            O::SkipCharsForward,
            O::SkipCharsBackward,
            O::ForwardLine,
            O::CharSyntax,
            O::BufferSubstring,
            O::DeleteRegion,
            O::NarrowToRegion,
            O::Widen,
            O::EndOfLine,
            O::SaveExcursion,
            O::SaveRestriction,
            O::UnwindProtect,
            O::SetMarker,
            O::MatchBeginning,
            O::MatchEnd,
            O::Upcase,
            O::Downcase,
            O::StringEqlSign,
            O::StringLessThan,
            O::InsertN,
        ] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            // Include complete operands even for PushCatch (u16) and
            // InsertN (u8): malformed encoding is a decoder error, not an
            // unsupported-operation signal, and is tested separately.
            make_bytecode!(bytecode, 0, [opcode, 0, 0, O::Return], [], cx);
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let error = call(bytecode, 0, "unsupported-opcode", frame, cx).unwrap_err();
            let ErrorType::Signal(id) = error.error else {
                panic!("Expected named signal for {opcode:?}");
            };
            let (symbol, data) = frame.get_exception(id).unwrap();
            assert_eq!(symbol.bind(cx), sym::RUNE_UNIMPLEMENTED);
            let data = data.bind(cx).cons().unwrap();
            assert_eq!(data.car(), cx.add(format!("Unsupported bytecode opcode: {opcode:?}")));
        }
    }

    #[test]
    fn m0_unimplemented_opcode_uses_existing_error_handler() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let condition = Cons::new1(sym::ERROR, cx);
        make_bytecode!(
            bytecode,
            0,
            [
                O::Constant0,
                O::PushCondtionCase,
                0x08,
                0x00,
                O::Substring,
                O::PopHandler,
                O::Constant1,
                O::Return,
                O::Car,
                O::Return
            ],
            [condition, NIL],
            cx
        );
        check_bytecode!(bytecode, [], sym::RUNE_UNIMPLEMENTED, cx);
    }

    #[test]
    fn test_basic() {
        use OpCode::*;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        // (lambda () 5)
        make_bytecode!(bytecode, 0, [Constant0, Return], [5], cx);
        check_bytecode!(bytecode, [], 5, cx);
        // (lambda (x) (+ x 5))
        make_bytecode!(bytecode, 257, [Duplicate, Constant0, Plus, Return], [5], cx);
        check_bytecode!(bytecode, [7], 12, cx);
        // (lambda (x &optional y) (+ x y))
        make_bytecode!(bytecode, 513, [StackRef1, StackRef1, Plus, Return], [], cx);
        check_bytecode!(bytecode, [3, 4], 7, cx);
        // (lambda (x) (if x 2 3))
        make_bytecode!(
            bytecode,
            257,
            [Duplicate, GotoIfNil, 0x06, 0x00, Constant0, Return, Constant1, Return],
            [2, 3],
            cx
        );
        check_bytecode!(bytecode, [false], 3, cx);
        check_bytecode!(bytecode, [true], 2, cx);

        // (lambda (x) (let ((y 0))
        //          (while (< 0 x)
        //            (setq x (1- x))
        //            (setq y (1+ y)))
        //          y))
        make_bytecode!(
            bytecode,
            257,
            [
                Constant0, Constant0, StackRef2, LessThan, GotoIfNil, VarSet2, 0x00, StackRef1,
                Sub1, StackSetN, 0x02, Duplicate, Add1, StackSetN, 0x01, Goto, 0x01, 0x00, Return
            ],
            [0],
            cx
        );
        check_bytecode!(bytecode, [5], 5, cx);
        check_bytecode!(bytecode, [0], 0, cx);
    }

    #[test]
    fn test_bytecode_call() {
        use OpCode::*;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        // (lambda (x) (symbol-name x))
        make_bytecode!(
            bytecode,
            257,
            [Constant0, StackRef1, Call1, Return],
            [sym::SYMBOL_NAME],
            cx
        );
        check_bytecode!(bytecode, [sym::SYMBOL_NAME], "symbol-name", cx);
        check_bytecode!(bytecode, [sym::AREF], "aref", cx);
        check_bytecode!(bytecode, [sym::ADD], "+", cx);

        // (lambda (x y z) (+ x y z))
        make_bytecode!(
            bytecode,
            771,
            [Constant0, StackRef3, StackRef3, StackRef3, Call3, Return],
            [sym::ADD],
            cx
        );
        check_bytecode!(bytecode, [1, 2, 3], 6, cx);

        // (lambda (&rest x) (apply '+ x))
        make_bytecode!(
            bytecode,
            128,
            [Constant0, Constant1, StackRef2, Call2, Return],
            [sym::APPLY, sym::ADD],
            cx
        );
        check_bytecode!(bytecode, [1, 2, 3], 6, cx);

        // (lambda (x &optional y) (+ x y))
        make_bytecode!(bytecode, 513, [StackRef1, StackRef1, Plus, Return], [], cx);
        check_bytecode!(bytecode, [1, 2], 3, cx);
    }

    #[test]
    fn test_bytecode_variables() {
        use OpCode::*;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();

        // (lambda () (let ((load-path 5)) load-path))
        make_bytecode!(
            bytecode,
            0,
            [Constant1, VarBind0, VarRef0, Unbind1, Return],
            [sym::LOAD_PATH, 5],
            cx
        );
        check_bytecode!(bytecode, [], 5, cx);
    }

    #[test]
    fn m0_switch_rejects_invalid_tables_and_targets() {
        use OpCode as O;
        for offset in [-1_i64, 65_536] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            let mut table = HashTable::default();
            table.insert(1.into(), offset.into());
            make_bytecode!(
                bytecode,
                0,
                [O::Constant0, O::Constant1, O::Switch, O::Constant2, O::Return],
                [1, table, 9],
                cx
            );
            root!(env, new(Env), cx);
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let before = frame.stack.len();
            assert!(call(bytecode, 0, "invalid-switch-target", frame, cx).is_err());
            assert_eq!(frame.stack.len(), before + 2);
        }
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let mut table = HashTable::default();
        table.insert(1.into(), cx.add("not-an-index"));
        make_bytecode!(bad_target, 0, [O::Constant0, O::Constant1, O::Switch], [1, table], cx);
        root!(env, new(Env), cx);
        {
            let frame = &mut CallFrame::new(env);
            frame.finalize_arguments();
            let before = frame.stack.len();
            assert!(call(bad_target, 0, "invalid-switch-type", frame, cx).is_err());
            assert_eq!(frame.stack.len(), before + 2);
        }
        make_bytecode!(bad_table, 0, [O::Constant0, O::Constant1, O::Switch], [1, 7], cx);
        let frame = &mut CallFrame::new(env);
        frame.finalize_arguments();
        let before = frame.stack.len();
        assert!(call(bad_table, 0, "invalid-switch-table", frame, cx).is_err());
        assert_eq!(frame.stack.len(), before + 2);
    }

    #[test]
    fn m0_switch_preserves_targets_above_u16_range() {
        use OpCode as O;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let mut table = HashTable::default();
        table.insert(1.into(), 65_536_i64.into());
        let constants: &LispVec =
            vec![cx.add(1_i64), cx.add(table), cx.add(9_i64)].into_obj(cx).untag();
        let mut bytes = vec![O::Return as u8; 65_538];
        bytes[..3].copy_from_slice(&[O::Constant0 as u8, O::Constant1 as u8, O::Switch as u8]);
        bytes[65_536..].copy_from_slice(&[O::Constant2 as u8, O::Return as u8]);
        let codes: &ByteString = bytes.into_obj(cx).untag();
        let function =
            crate::alloc::make_byte_code(0, codes, constants, 10, None, None, &[], cx).unwrap();
        root!(function, cx);
        check_bytecode!(function, [], 9, cx);
    }

    #[test]
    fn test_bytecode_advanced() {
        use OpCode::*;
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);

        let mut table = HashTable::default();
        table.insert(1.into(), 6.into());
        table.insert(2.into(), 8.into());
        table.insert(3.into(), 10.into());

        // (lambda (n)
        //   (cond ((equal n 1) 1)
        //         ((equal n 2) 2)
        //         ((equal n 3) 3)))
        make_bytecode!(
            bytecode,
            257,
            [
                Duplicate, Constant0, Switch, Goto, 0x0C, 0x00, Constant1, Return, Constant2,
                Return, Constant3, Return, Constant4, Return
            ],
            [table, 4, 5, 6, false],
            cx
        );
        check_bytecode!(bytecode, [1], 4, cx);
        check_bytecode!(bytecode, [2], 5, cx);
        check_bytecode!(bytecode, [3], 6, cx);
        check_bytecode!(bytecode, [4], false, cx);

        // (1+ (let ((a 1) (_b) (_c)) a))
        make_bytecode!(
            bytecode,
            0,
            [Constant0, Constant1, Duplicate, StackRef2, DiscardN, 0x83, Add1, Return],
            [1, false],
            cx
        );
        check_bytecode!(bytecode, [], 2, cx);

        // (lambda () (list 1 2 3 4 5 6))
        make_bytecode!(
            bytecode,
            0,
            [
                Constant0, Constant1, Constant2, Constant3, Constant4, Constant5, ListN, 6, Return
            ],
            [1, 2, 3, 4, 5, 6],
            cx
        );
        let list = list![1, 2, 3, 4, 5, 6; cx];
        root!(list, cx);
        check_bytecode!(bytecode, [], list, cx);

        // hand rolled bytecode
        // (lambda () (list 1 2 3 4 5 6) 7)
        make_bytecode!(
            bytecode,
            0,
            [
                Constant6, Constant0, Constant1, Constant2, Constant3, Constant4, Constant5, ListN,
                6, DiscardN, 1, Return
            ],
            [1, 2, 3, 4, 5, 6, 7],
            cx
        );
        check_bytecode!(bytecode, [], 7, cx);
    }

    #[test]
    fn test_handlers() {
        use OpCode as O;

        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let err = Cons::new1(sym::ERROR, cx);

        // (lambda (y) (condition-case nil
        //            (floor)
        //              (error (+ y 4))))
        make_bytecode!(
            bytecode,
            257,
            [
                O::Constant0,
                O::PushCondtionCase,
                0x09,
                0x0,
                O::Constant1,
                O::StackRef1,
                O::Call1,
                O::PopHandler,
                O::Return,
                O::Discard,
                O::Duplicate,
                O::Constant2,
                O::Plus,
                O::Return
            ],
            [err, sym::SYMBOL_NAME, 4],
            cx
        );
        check_bytecode!(bytecode, [3], 7, cx);
        check_bytecode!(bytecode, [sym::FLOOR], "floor", cx);
    }

    #[test]
    fn test_recursive_handlers() {
        use OpCode as O;

        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        let err = Cons::new1(sym::ERROR, cx);

        // (lambda () (floor))
        // ;; This will error out
        make_bytecode!(inner, 0, [O::Constant0, O::Call0, O::Return], [sym::FLOOR], cx);

        // (lambda (x)
        //    (condition-case nil
        //        (funcall x)
        //      (error 7)))
        make_bytecode!(
            outer,
            257,
            [
                O::Constant0,
                O::PushCondtionCase,
                0x08,
                0x0,
                O::Duplicate,
                O::Call0,
                O::PopHandler,
                O::Return,
                O::Discard,
                O::Constant1,
                O::Return
            ],
            [err, 7],
            cx
        );
        let inner = cx.add(inner.bind(cx));
        root!(inner, cx);
        check_bytecode!(outer, [inner], 7, cx);
    }
}
