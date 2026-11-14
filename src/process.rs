//! Minimal real pipe subprocess primitives; native I/O and GC registry are separate.
use crate::core::{
    env::{Env, intern},
    gc::{Context, Rt},
    object::{Gc, LispProcess, NIL, Object, ObjectType, ProcessCoding, Symbol},
};
use crate::process_transport::{ProcessStatus, ProcessTransport, SpawnSpec};
use anyhow::{Result, bail};
use rune_macros::defun;
use rune_core::macros::list;
use std::ffi::OsString;

// GNU resolves bare program names through owner `exec-path`, not the child
// PATH. Until that owner variable is supported, reject relative programs
// instead of allowing Command's platform/ambient search to execute another file.
const MAX_NAME_BYTES:usize=65536;
const MAX_ARGUMENT_BYTES:usize=1024*1024;
fn type_error(predicate: Symbol, value: Object, cx: &Context) -> anyhow::Error {
    crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(),list!(predicate,value;cx),cx).into()
}
fn string_bytes<'a>(value:Object<'a>,limit:usize,cx:&Context)->Result<&'a [u8]> {
    let bytes=match value.untag(){ObjectType::String(s)=>s.as_bytes(),ObjectType::ByteString(s)=>&s,_=>return Err(type_error(crate::sym::STRINGP,value,cx))};
    anyhow::ensure!(bytes.len()<=limit,"Native process string exceeds byte limit");
    Ok(bytes)
}
fn process_name_text<'a>(value:Object<'a>,cx:&Context)->Result<&'a str> {
    Ok(std::str::from_utf8(string_bytes(value,MAX_NAME_BYTES,cx)?)?)
}
fn absolute_program(program:Object,cx:&Context)->Result<OsString> {
    let program=native_string(program,MAX_NAME_BYTES,cx)?;
    if !std::path::Path::new(&program).is_absolute() {
        bail!("Subprocess program must be an absolute path; owner exec-path search is not implemented");
    }
    Ok(program)
}
fn native_string(value:Object,limit:usize,cx:&Context)->Result<OsString> {
    let bytes=string_bytes(value,limit,cx)?;
    anyhow::ensure!(!bytes.contains(&0),"Native process argument contains NUL");
    #[cfg(unix)] { use std::os::unix::ffi::OsStringExt; Ok(OsString::from_vec(bytes.to_vec())) }
    #[cfg(not(unix))] { Ok(OsString::from(std::str::from_utf8(bytes)?)) }
}
fn arguments(values:&[Object],cx:&Context)->Result<Vec<OsString>> {
    anyhow::ensure!(values.len()<=4096,"Native process argument count exceeds limit");
    let mut size=0usize;
    // Validate the entire borrowed argv before allocating native copies.
    for &value in values { let bytes=string_bytes(value,MAX_ARGUMENT_BYTES,cx)?;anyhow::ensure!(!bytes.contains(&0),"Native process argument contains NUL");size=size.checked_add(bytes.len()).ok_or_else(||anyhow::anyhow!("Native argv size overflow"))?;anyhow::ensure!(size<=16*1024*1024,"Native argv exceeds 16 MiB limit"); }
    let mut output=Vec::new();output.try_reserve_exact(values.len())?;
    for &value in values { output.push(native_string(value,MAX_ARGUMENT_BYTES,cx)?); }
    Ok(output)
}
fn buffer_operand<'a>(value:Object<'a>,cx:&'a Context)->Result<Object<'a>> {
    match value.untag() {
        ObjectType::NIL | ObjectType::Buffer(_)=>Ok(value),
        ObjectType::String(_)=>{process_name_text(value,cx)?;Ok(value)},
        ObjectType::ByteString(_)=>Ok(cx.add(process_name_text(value,cx)?.to_owned())),
        _=>Err(type_error(crate::sym::STRINGP,value,cx)),
    }
}

// Copy owner variable storage before starting workers; never mutate host cwd.
fn owner_working_directory(env: &Rt<Env>, cx: &Context) -> Result<std::path::PathBuf> {
    let value=crate::data::symbol_value(crate::sym::DEFAULT_DIRECTORY,env,cx)?;
    let bytes: &[u8]=match value.untag() {
        ObjectType::String(s)=>s.as_bytes(),
        ObjectType::ByteString(s)=>&s,
        _=>return Err(crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(),rune_core::macros::list!(crate::sym::STRINGP,value;cx),cx).into()),
    };
    anyhow::ensure!(bytes.len()<=65536,"Subprocess working directory exceeds 64 KiB limit");
    anyhow::ensure!(!bytes.contains(&0),"Subprocess working directory contains NUL");
    #[cfg(unix)]
    let path={use std::os::unix::ffi::OsStrExt;std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))};
    #[cfg(not(unix))]
    let path=std::path::PathBuf::from(std::str::from_utf8(bytes)?);
    anyhow::ensure!(path.is_absolute(),"Subprocess working directory must be absolute");
    anyhow::ensure!(std::fs::metadata(&path)?.is_dir(),"Subprocess working directory is not a directory");
    Ok(path)
}

fn resolve<'ob>(object:Object<'ob>,cx:&'ob Context)->Result<Gc<&'ob LispProcess>> {
    let value=match object.untag(){
        ObjectType::Process(_)=>object,
        ObjectType::String(_) | ObjectType::ByteString(_)=>{
            let name=process_name_text(object,cx)?;
            let root=cx.find_process(name).ok_or_else(||anyhow::anyhow!("No process with requested name"))?;
            let value=root.bind(cx)?;
            if !matches!(value.untag(),ObjectType::Process(_)) {bail!("Process registry contains a non-process")};
            value
        },
        _=>return Err(type_error(crate::sym::PROCESSP,object,cx)),
    };
    // Tag checked before conversion: its diagnostic fallback is unreachable.
    let process:Gc<&LispProcess>=value.try_into()?;
    process.transport(cx)?;Ok(process)
}
fn coding(object: Option<Object>) -> Result<ProcessCoding> {
    match object.map(Object::untag) {
        None | Some(ObjectType::NIL) => Ok(ProcessCoding::Binary),
        Some(ObjectType::Symbol(symbol)) => match symbol.name() {
            "binary" | "no-conversion" => Ok(ProcessCoding::Binary),
            "utf-8" => Ok(ProcessCoding::Utf8),
            "utf-8-unix" => Ok(ProcessCoding::Utf8Unix),
            other => bail!("Unsupported subprocess coding system: {other}"),
        },
        _ => bail!("Expected a subprocess coding-system symbol"),
    }
}
fn coding_symbol<'ob>(value: ProcessCoding, cx: &'ob Context) -> Symbol<'ob> {
    intern(
        match value {
            ProcessCoding::Binary => "binary",
            ProcessCoding::Utf8 => "utf-8",
            ProcessCoding::Utf8Unix => "utf-8-unix",
        },
        cx,
    )
}

#[defun]
fn start_process<'ob>(
    name: Object,
    buffer: Object<'ob>,
    program: Object,
    args: &[Object],
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let buffer=buffer_operand(buffer,cx)?;
    if !matches!(name.untag(),ObjectType::String(_) | ObjectType::ByteString(_)) {
        return Err(crate::eval::EvalError::signal(crate::sym::ERROR.into(),list!(cx.add(":name value not a string");cx),cx).into());
    }
    let name=process_name_text(name,cx)?;
    let program=absolute_program(program,cx)?;
    let args=arguments(args,cx)?;
    let cwd=owner_working_directory(env,cx)?;
    let buffer = if buffer.is_nil() {
        NIL
    } else {
        crate::buffer::get_buffer_create(buffer_operand(buffer,cx)?, None, cx)?
    };

    let mut actual = name.to_owned();
    let mut suffix = 1_u64;
    while cx.find_process(&actual).is_some() {
        actual = format!("{name}<{suffix}>");
        suffix = suffix
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Process name space exhausted"))?;
    }
    let native = ProcessTransport::spawn_exact_environment(SpawnSpec {
        program,
        args,
        cwd: Some(cwd),
        env: crate::emacs::process_environment_snapshot(env, cx)?,
    })?;
    // SAFETY: buffer came from the current Context's editor gateway or NIL;
    // callbacks are NIL. No Lisp value crosses into native transport workers.
    unsafe { cx.register_process(actual, native, NIL, NIL, buffer) }
}
#[defun]
fn processp(object: Object) -> bool {
    matches!(object.untag(), ObjectType::Process(_))
}
#[defun]
fn get_process<'ob>(name: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    match name.untag() {
        ObjectType::Process(_) => {
            resolve(name, cx)?;
            Ok(name)
        }
        ObjectType::String(_) | ObjectType::ByteString(_)=>cx.find_process(process_name_text(name,cx)?).map_or(Ok(NIL),|root|root.bind(cx)),
        _=>Err(type_error(crate::sym::STRINGP,name,cx)),
    }
}
#[defun]
fn process_list<'ob>(cx: &'ob Context) -> Result<Object<'ob>> {
    let objects = cx
        .process_list_roots()
        .iter()
        .map(|root| root.bind(cx))
        .collect::<Result<Vec<_>>>()?;
    Ok(crate::alloc::list(&objects, cx))
}
#[defun]
fn process_name(process: Object, cx: &Context) -> Result<String> {
    Ok(resolve(process, cx)?.name().to_owned())
}
#[defun]
fn process_id(process: Object, cx: &Context) -> Result<i64> {
    Ok(i64::from(resolve(process, cx)?.transport(cx)?.id()))
}
#[defun]
fn process_status<'ob>(process: Object, cx: &'ob Context) -> Result<Symbol<'ob>> {
    let status = resolve(process, cx)?.transport(cx)?.status();
    Ok(intern(
        match status {
            ProcessStatus::Running => "run",
            ProcessStatus::Exited(exit) if exit.signal.is_some() => "signal",
            ProcessStatus::Exited(_) => "exit",
        },
        cx,
    ))
}
#[defun]
fn process_exit_status(process: Object, cx: &Context) -> Result<i64> {
    Ok(match resolve(process, cx)?.transport(cx)?.status() {
        ProcessStatus::Running => 0,
        ProcessStatus::Exited(exit) => i64::from(exit.code.or(exit.signal).unwrap_or(0)),
    })
}
#[defun]
fn process_live_p(process: Object, cx: &Context) -> Result<bool> {
    if process.is_nil() {
        return Ok(false);
    }
    if matches!(process.untag(),ObjectType::String(_) | ObjectType::ByteString(_)) {
        if cx.find_process(process_name_text(process,cx)?).is_none() {
            return Ok(false);
        }
    }
    Ok(matches!(resolve(process, cx)?.transport(cx)?.status(), ProcessStatus::Running))
}
#[defun]
fn process_buffer<'ob>(process: Object, cx: &'ob Context) -> Result<Object<'ob>> {
    resolve(process, cx)?.callback_snapshot(cx)?.buffer.bind(cx)
}
/// Only owner-local, live process records are searched. Snapshot roots keep
/// process/buffer objects alive without retaining registry guards across work.
#[defun]
fn get_buffer_process<'ob>(
    buffer: Object<'ob>,
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let handle = if buffer.is_nil() {
        env.current_buffer_handle()
    } else {
        let value = crate::buffer::get_buffer(buffer_operand(buffer,cx)?, cx)?;
        if value.is_nil() {
            return Ok(NIL);
        }
        let ObjectType::Buffer(buffer) = value.untag() else { unreachable!() };
        buffer.read(cx)?;
        buffer.handle()
    };
    for snapshot in cx.process_snapshots() {
        if snapshot.transport.status() != ProcessStatus::Running {
            continue;
        }
        let value = snapshot.buffer.bind(cx)?;
        if matches!(value.untag(), ObjectType::Buffer(buffer) if buffer.handle() == handle) {
            return snapshot.process.bind(cx);
        }
    }
    Ok(NIL)
}

#[defun]
fn process_filter<'ob>(process: Object, cx: &'ob Context) -> Result<Object<'ob>> {
    let value = resolve(process, cx)?.callback_snapshot(cx)?.filter.bind(cx)?;
    Ok(if value.is_nil() {
        intern("internal-default-process-filter", cx).into()
    } else {
        value
    })
}
#[defun]
fn process_sentinel<'ob>(process: Object, cx: &'ob Context) -> Result<Object<'ob>> {
    let value = resolve(process, cx)?.callback_snapshot(cx)?.sentinel.bind(cx)?;
    Ok(if value.is_nil() {
        intern("internal-default-process-sentinel", cx).into()
    } else {
        value
    })
}
#[defun]
fn process_plist<'ob>(process: Object, cx: &'ob Context) -> Result<Object<'ob>> {
    resolve(process, cx)?.callback_snapshot(cx)?.plist.bind(cx)
}
#[defun]
fn set_process_filter<'ob>(
    process: Object,
    filter: Object<'ob>,
    cx: &Context,
) -> Result<Object<'ob>> {
    let process = resolve(process, cx)?;
    // SAFETY: arguments were evaluated in this owner Context and remain live;
    // only the registry receives Lisp values, never native worker threads.
    unsafe {
        cx.set_process_filter(&process, filter)?;
    }
    Ok(if filter.is_nil() {
        intern("internal-default-process-filter", cx).into()
    } else {
        filter
    })
}
#[defun]
fn set_process_sentinel<'ob>(
    process: Object,
    sentinel: Object<'ob>,
    cx: &Context,
) -> Result<Object<'ob>> {
    let process = resolve(process, cx)?;
    // SAFETY: same-owner evaluated argument; stable registry root before return.
    unsafe {
        cx.set_process_sentinel(&process, sentinel)?;
    }
    Ok(if sentinel.is_nil() {
        intern("internal-default-process-sentinel", cx).into()
    } else {
        sentinel
    })
}
#[defun]
fn set_process_buffer<'ob>(
    process: Object,
    buffer: Object<'ob>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let process = resolve(process, cx)?;
    let buffer = if buffer.is_nil() {
        NIL
    } else {
        crate::buffer::get_buffer_create(buffer_operand(buffer,cx)?, None, cx)?
    };
    // SAFETY: newly resolved owner buffer or NIL, no callback/collection here.
    unsafe {
        cx.set_process_buffer(&process, buffer)?;
    }
    Ok(buffer)
}
#[defun]
fn process_get<'ob>(
    process: Object,
    property: Object<'ob>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    crate::fns::plist_get(resolve(process, cx)?.callback_snapshot(cx)?.plist.bind(cx)?, property)
}
#[defun]
fn process_put<'ob>(
    process: Object,
    property: Object<'ob>,
    value: Object<'ob>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    let process = resolve(process, cx)?;
    let plist = crate::fns::plist_put(
        process.callback_snapshot(cx)?.plist.bind(cx)?,
        property,
        value,
        None,
        cx,
    )?;
    // SAFETY: plist is made only from this call's same-owner values.
    unsafe {
        cx.set_process_plist(&process, plist)?;
    }
    Ok(value)
}
#[defun]
fn set_process_coding_system(
    process: Object,
    decoding: Option<Object>,
    encoding: Option<Object>,
    cx: &Context,
) -> Result<()> {
    let process = resolve(process, cx)?;
    let read = coding(decoding)?;
    let write = coding(encoding)?;
    let previous = cx.process_state(&process)?;
    if previous.read_coding != read
        && (!previous.pending_utf8.is_empty() || !previous.stderr_pending_utf8.is_empty())
    {
        bail!("Cannot change process coding with an incomplete UTF8 sequence pending");
    }
    cx.update_process_state(&process, |state| {
        state.read_coding = read;
        state.write_coding = write;
    })?;
    Ok(())
}
#[defun]
fn process_coding_system<'ob>(process: Object, cx: &'ob Context) -> Result<Object<'ob>> {
    let process = resolve(process, cx)?;
    let state = cx.process_state(&process)?;
    Ok(crate::core::cons::Cons::new(
        coding_symbol(state.read_coding, cx),
        coding_symbol(state.write_coding, cx),
        cx,
    )
    .into())
}
#[defun]
fn set_process_query_on_exit_flag<'ob>(
    process: Object,
    flag: Object<'ob>,
    cx: &Context,
) -> Result<Object<'ob>> {
    let process = resolve(process, cx)?;
    cx.update_process_state(&process, |state| state.query_on_exit = !flag.is_nil())?;
    Ok(flag)
}
#[defun]
fn process_query_on_exit_flag(process: Object, cx: &Context) -> Result<bool> {
    let process = resolve(process, cx)?;
    Ok(cx.process_state(&process)?.query_on_exit)
}
#[defun]
fn process_send_string(process: Object, string: Object, cx: &Context) -> Result<()> {
    string_bytes(string,MAX_ARGUMENT_BYTES,cx)?;
    let process = resolve(process, cx)?;
    let coding = cx.process_state(&process)?.write_coding;
    match string.untag() {
        ObjectType::String(value) => {
            if coding == ProcessCoding::Binary && !value.is_ascii() {
                bail!("Binary subprocess write requires a byte string or ASCII text");
            }
            process.transport(cx)?.send(value.as_bytes())?;
        }
        ObjectType::ByteString(value) => {
            if coding != ProcessCoding::Binary {
                std::str::from_utf8(&value)?;
            }
            process.transport(cx)?.send(&value)?;
        }
        _ => return Err(type_error(crate::sym::STRINGP,string,cx)),
    }
    Ok(())
}
#[defun]
fn process_send_eof<'ob>(process: Object<'ob>, cx: &'ob Context) -> Result<Object<'ob>> {
    resolve(process, cx)?.transport(cx)?.close_stdin();
    Ok(process)
}
#[defun]
fn delete_process(process: Object, cx: &Context) -> Result<()> {
    // Registry stays live for EOF/exit/filter/sentinel owner-pump delivery.
    resolve(process, cx)?.transport(cx)?.terminate();
    Ok(())
}

/// Default insertion is intentionally UTF8-only because editor buffers have a
/// UTF8 content model. Binary filters must consume their byte string explicitly.
#[defun]
pub(crate) fn internal_default_process_filter(
    process: Object,
    string: Object,
    cx: &Context,
) -> Result<()> {
    let process = resolve(process, cx)?;
    string_bytes(string,MAX_ARGUMENT_BYTES,cx)?;
    let snapshot = process.callback_snapshot(cx)?;
    let buffer = snapshot.buffer.bind(cx)?;
    if buffer.is_nil() {
        return Ok(());
    }
    let ObjectType::Buffer(buffer) = buffer.untag() else {
        bail!("Process buffer is not a buffer");
    };
    let text = match string.untag() {
        ObjectType::String(value) => (**value).to_owned(),
        ObjectType::ByteString(value) => std::str::from_utf8(&value)?.to_owned(),
        _ => return Err(type_error(crate::sym::STRINGP,string,cx)),
    };
    let read = buffer.read(cx)?;
    let end = read.document.text().len_chars();
    cx.editor_gateway()?.splice(read.handle, read.epoch, end..end, &text, cx)?;
    Ok(())
}
#[defun(name = "internal-default-process-sentinel")]
fn internal_default_process_sentinel_lisp(process:Object,event:Object,cx:&Context)->Result<()> {
    resolve(process,cx)?;
    let event=process_name_text(event,cx)?;
    internal_default_process_sentinel(process,event,cx)
}
pub(crate) fn internal_default_process_sentinel(
    process: Object,
    event: &str,
    cx: &Context,
) -> Result<()> {
    let name = resolve(process, cx)?.name().to_owned();
    let text = cx.add(format!("\nProcess {name} {event}"));
    internal_default_process_filter(process, text, cx)
}

/// Synchronous pipe subprocess for real version checks and simple commands.
/// No shell is introduced here: a shell runs only when explicitly PROGRAM.
#[defun]
fn call_process<'ob>(
    program: Object,
    infile: Option<Object>,
    destination: Option<Object<'ob>>,
    _display: Option<Object>,
    args: &[Object],
    env: &Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    use crate::process_transport::ProcessEvent;
    use std::time::{Duration, Instant};
    let program=absolute_program(program,cx)?;
    if let Some(value)=infile.filter(|v|!v.is_nil()) {
        string_bytes(value,MAX_NAME_BYTES,cx)?;
        bail!("call-process input files are not implemented; stdin must be nil");
    }
    let args=arguments(args,cx)?;
    let cwd=owner_working_directory(env,cx)?;
    let destination=destination.unwrap_or(NIL);
    let destination=if matches!(destination.untag(),ObjectType::TRUE){destination}else{buffer_operand(destination,cx)?};
    // Validate destination before starting a real executable.
    let output_buffer = match destination.untag() {
        ObjectType::NIL => None,
        ObjectType::TRUE => Some(env.current_buffer_handle()),
        ObjectType::Buffer(buffer) => {
            buffer.read(cx)?;
            Some(buffer.handle())
        }
        ObjectType::String(_) => {
            let buffer = crate::buffer::get_buffer_create(destination, None, cx)?;
            let ObjectType::Buffer(buffer) = buffer.untag() else { unreachable!() };
            Some(buffer.handle())
        }
        _ => bail!("call-process destination must be nil, t, a buffer or a buffer name"),
    };

    let native = ProcessTransport::spawn_merged_exact_environment(SpawnSpec {
        program,
        args,
        cwd: Some(cwd),
        env: crate::emacs::process_environment_snapshot(env, cx)?,
    })?;
    native.close_stdin();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut output, mut stdout_eof, mut stderr_eof, mut exit) = (Vec::new(), false, false, None);
    let outcome = (|| -> Result<()> {
        loop {
            env.poll_quit(cx)?;
            if Instant::now() >= deadline {
                bail!("call-process exceeded its 10-second deadline");
            }
            match native.recv(Duration::from_millis(20)) {
                Some(ProcessEvent::Output { bytes, .. }) => {
                    if output.len().checked_add(bytes.len()).is_none_or(|n| n > 1024 * 1024) {
                        bail!("call-process output exceeds 1 MiB");
                    }
                    output.extend(bytes);
                }
                Some(ProcessEvent::Eof(crate::process_transport::OutputStream::Stdout)) => {
                    stdout_eof = true
                }
                Some(ProcessEvent::Eof(crate::process_transport::OutputStream::Stderr)) => {
                    stderr_eof = true
                }
                Some(ProcessEvent::Exit(value)) => exit = Some(value),
                Some(ProcessEvent::Failure(error)) => bail!("call-process I/O failed: {error}"),
                Some(ProcessEvent::LimitReached) => {
                    bail!("call-process transport output limit exceeded")
                }
                None => {}
            }
            if stdout_eof && stderr_eof && exit.is_some() {
                return Ok(());
            }
        }
    })();
    if outcome.is_err() {
        native.terminate();
    }
    outcome?;
    if let Some(buffer) = output_buffer {
        let text = std::str::from_utf8(&output)?;
        let gateway = cx.editor_gateway()?;
        let read = gateway.read(buffer, cx)?;
        gateway.splice(read.handle, read.epoch, read.point..read.point, text, cx)?;
    }
    let exit = exit.unwrap();
    Ok(match exit.code {
        Some(code) => cx.add(i64::from(code)),
        None => cx.add(format!("signal {}", exit.signal.unwrap_or(0))),
    })
}

/// Deliberately restricted to this owner Context's actual child processes.
/// Numeric PIDs are looked up in that registry, never signalled directly.
#[defun]
fn signal_process(process: Object, signal: Object, cx: &Context) -> Result<i64> {
    let number = match signal.untag() {
        ObjectType::Int(value) => i32::try_from(value)?,
        ObjectType::Symbol(value) => match value.name() {
            "SIGINT" | "INT" => 2,
            "SIGKILL" | "KILL" => 9,
            "SIGTERM" | "TERM" => 15,
            other => bail!("Unsupported process signal: {other}"),
        },
        _ => bail!("Expected a supported process signal integer or symbol"),
    };
    let native = if let ObjectType::Int(pid) = process.untag() {
        let pid = u32::try_from(pid)?;
        cx.process_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.transport.id() == pid)
            .ok_or_else(|| anyhow::anyhow!("PID is not an owned child process"))?
            .transport
    } else {
        resolve(process, cx)?.transport(cx)?
    };
    native.signal(number)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use crate::interpreter::assert_lisp;
    #[test]
    fn child_environment_is_complete_and_first_entry_wins() {
        assert_lisp(
            r#"(progn (set-buffer (get-buffer-create "rune-empty-child-env"))
                (let ((process-environment nil))
                  (list (call-process "/usr/bin/env" nil t)
                    (buffer-substring-no-properties (point-min) (point-max)))))"#,
            r#"(0 "")"#,
        );
        assert_lisp(
            r#"(progn (set-buffer (get-buffer-create "rune-ordered-child-env"))
                (let ((process-environment '("A=first" "A=second" "B" "B=later" "C=" "PATH=/not/used/for/program/search")))
                  (list (call-process "/usr/bin/env" nil t)
                    (buffer-substring-no-properties (point-min) (point-max)))))"#,
            r#"(0 "A=first\nC=\nPATH=/not/used/for/program/search\n")"#,
        );
    }
    #[test]
    fn asynchronous_child_uses_same_owner_environment_snapshot() {
        assert_lisp(
            r#"(let ((process-environment '("ASYNC_OWNER=first" "ASYNC_OWNER=later"))
                     (p nil) (attempts 0) (done nil))
                (set-buffer (get-buffer-create "rune-async-child-env"))
                (setq p (start-process "rune-async-child-env" (current-buffer) "/usr/bin/env"))
                (set-process-sentinel p #'(lambda (process event) (setq done t)))
                (unwind-protect
                    (progn
                      (while (and (null done) (< attempts 200))
                        (setq attempts (+ attempts 1)) (accept-process-output p 0.01))
                      (accept-process-output p 0)
                      (list (process-exit-status p)
                        (buffer-substring-no-properties (point-min) (point-max))))
                  (delete-process p)))"#,
            r#"(0 "ASYNC_OWNER=first\n")"#,
        );
    }

    #[test]
    fn child_environment_rejects_relative_program_and_nul_before_spawn() {
        assert_lisp(
            r#"(let ((process-environment '("PATH=/usr/bin")))
                (list (condition-case nil (call-process "env") (error 'caught))
                      (condition-case nil (start-process "relative-must-not-spawn" nil "env") (error 'caught))
                      (null (get-process "relative-must-not-spawn"))))"#,
            "(caught caught t)",
        );
        assert_lisp(
            r#"(let ((process-environment (list (concat "A=" (string 0)))))
                (list (condition-case nil (start-process "nul-must-not-spawn" nil "/usr/bin/env") (error 'caught))
                      (null (get-process "nul-must-not-spawn"))))"#,
            "(caught t)",
        );
    }

    #[test]
    fn genuine_process_metadata_roots_survive_collection() {
        assert_lisp(
            r#"(let ((p (start-process "rune-process-metadata" " *rune-process-output*" "/usr/bin/python3" "-c" "import time; time.sleep(30)")))
            (unwind-protect
                (progn
                    (process-put p 'key '(1 2))
                    (set-process-query-on-exit-flag p nil)
                    (set-process-coding-system p 'binary 'utf-8-unix)
                    (set-process-filter p #'identity)
                    (garbage-collect)
                    (list (processp p) (eq p (get-process "rune-process-metadata"))
                        (process-name p) (process-coding-system p)
                        (process-query-on-exit-flag p) (process-get p 'key)
                        (bufferp (process-buffer p)) (> (process-id p) 0)
                        (eq (process-filter p) #'identity)))
                (delete-process p)))"#,
            r#"(t t "rune-process-metadata" (binary . utf-8-unix) nil (1 2) t t t)"#,
        );
    }
    #[test]
    fn process_names_are_unique_and_argv_validation_precedes_spawn() {
        assert_lisp(
            r#"(let ((a (start-process "rune-name" nil "/usr/bin/python3" "-c" "import time; time.sleep(30)")))
            (unwind-protect
                (let ((b (start-process "rune-name" nil "/usr/bin/python3" "-c" "import time; time.sleep(30)")))
                    (unwind-protect (list (process-name a) (process-name b)) (delete-process b)))
                (delete-process a)))"#,
            r#"("rune-name" "rune-name<1>")"#,
        );
        assert_lisp(
            r#"(condition-case nil (start-process "bad" nil "/usr/bin/python3" 3) (error 'caught))"#,
            "caught",
        );
    }
    #[test]
    fn synchronous_call_process_runs_real_argv_and_preserves_merged_order() {
        assert_lisp(
            r#"(progn (set-buffer (get-buffer-create "rune-call-process"))
            (let ((status (call-process "/usr/bin/python3" nil t nil "-c" "import os,sys; os.write(1,b'out'); os.write(2,b'err'); os.write(1,sys.argv[1].encode()); raise SystemExit(7)" "$(not-executed);literal")))
                (list status (buffer-substring-no-properties (point-min) (point-max)))))"#,
            r#"(7 "outerr$(not-executed);literal")"#,
        );
        assert_lisp(
            r#"(progn (set-buffer (get-buffer-create "rune-call-shell")) (list (call-process "/bin/sh" nil t nil "-c" "printf '%s' \"$1\"" "rune" "a b;$()") (buffer-substring-no-properties (point-min) (point-max))))"#,
            r#"(0 "a b;$()")"#,
        );
    }
    #[test]
    fn synchronous_output_limit_fails_without_partial_buffer_publication() {
        assert_lisp(
            r#"(progn (set-buffer (get-buffer-create "rune-call-limit"))
            (list (condition-case nil (call-process "/usr/bin/python3" nil t nil "-c" "import os; os.write(1,b'x'*2000000)") (error 'caught))
                (buffer-substring-no-properties (point-min) (point-max))))"#,
            r#"(caught "")"#,
        );
    }
    #[test]
    #[ignore = "explicit 10-second synchronous child deadline smoke"]
    fn synchronous_deadline_cancels_real_sleeping_child() {
        let start = std::time::Instant::now();
        assert_lisp(
            r#"(condition-case nil (call-process "/usr/bin/python3" nil nil nil "-c" "import time; time.sleep(30)") (error 'caught))"#,
            "caught",
        );
        assert!(start.elapsed() >= std::time::Duration::from_secs(10));
        assert!(start.elapsed() < std::time::Duration::from_secs(15));
    }
    #[test]
    #[ignore = "requires explicit official synthetic mu binary"]
    fn synchronous_actual_mu_version_handshake_is_not_faked() {
        let binary = std::env::var("RUNE_MU_BINARY").expect("official mu fixture wrapper");
        assert!(!binary.contains(['"', '\\', '\n']));
        assert_lisp(
            &format!(
                r#"(progn (set-buffer (get-buffer-create "rune-call-mu-version")) (list (call-process "{binary}" nil t nil "--version") (buffer-substring-no-properties (point-min) (point-max))))"#
            ),
            "(0 \"version 1.12.9\n\")",
        );
    }
    #[test]
    fn coding_change_rejects_partial_stderr_utf8_without_state_loss() {
        use crate::{
            core::{
                env::sym,
                gc::{Context, RootSet},
                object::{Gc, LispProcess, NIL, ProcessCoding},
            },
            process_transport::{ProcessTransport, SpawnSpec},
        };
        sym::init_symbols();
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let native = ProcessTransport::spawn(SpawnSpec {
            program: "/usr/bin/python3".into(),
            args: vec!["-c".into(), "import time; time.sleep(30)".into()],
            cwd: None,
            env: vec![],
        })
        .unwrap();
        // SAFETY: all payload values are global NIL; process is allocated here.
        let object =
            unsafe { cx.register_process("coding-carry".into(), native, NIL, NIL, NIL) }.unwrap();
        let process: Gc<&LispProcess> = object.try_into().unwrap();
        cx.update_process_state(&process, |state| {
            state.read_coding = ProcessCoding::Utf8;
            state.stderr_pending_utf8 = vec![0xce];
        })
        .unwrap();
        let binary = crate::core::env::intern("binary", &cx).into();
        assert!(super::set_process_coding_system(object, Some(binary), Some(binary), &cx).is_err());
        let state = cx.process_state(&process).unwrap();
        assert_eq!(state.read_coding, ProcessCoding::Utf8);
        assert_eq!(state.stderr_pending_utf8, vec![0xce]);
    }
    #[test]
    fn get_buffer_process_finds_owner_rooted_identity_after_gc() {
        assert_lisp(
            r#"(let ((p (start-process "rune-buffer-process" " *rune-buffer-process*" "/usr/bin/python3" "-c" "import time; time.sleep(30)")))
            (unwind-protect
                (progn (set-buffer (process-buffer p)) (garbage-collect)
                    (list (eq p (get-buffer-process nil))
                        (eq p (get-buffer-process " *rune-buffer-process*"))
                        (eq p (get-buffer-process (process-buffer p)))
                        (get-buffer-process "rune-never-created-buffer")
                        (get-buffer "rune-never-created-buffer")))
                (delete-process p)))"#,
            "(t t t nil nil)",
        );
    }
    #[test]
    fn arbitrary_unregistered_pid_signalling_is_rejected() {
        use crate::core::{
            env::sym,
            gc::{Context, RootSet},
        };
        sym::init_symbols();
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let target = cx.add(i64::from(std::process::id()));
        let signal = crate::core::env::intern("SIGKILL", &cx).into();
        // Empty owner registry means no OS signal can be dispatched.
        assert!(super::signal_process(target, signal, &cx).is_err());
    }
}

#[cfg(all(test,unix))]
mod owner_cwd_tests {
    #[test]
    fn owner_directory_snapshot_is_bounded_and_raw_bytes_survive_gc() {
        use crate::core::{env::{Env,sym},gc::{Context,RootSet}};
        use rune_core::macros::root;
        use std::os::unix::ffi::{OsStrExt,OsStringExt};
        static NEXT:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(0);
        let directory=loop {
            let n=NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
            let p=std::env::temp_dir().join(format!("rune-cwd-snapshot-{}-{n}",std::process::id()));
            match std::fs::create_dir(&p) { Ok(())=>break p,Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>continue,Err(e)=>panic!("Exclusive fixture directory failed: {e}") }
        };
        struct Cleanup(std::path::PathBuf);impl Drop for Cleanup {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
        let _cleanup=Cleanup(directory.clone());
        let raw=directory.join(std::ffi::OsString::from_vec(vec![0xff,0xfe]));std::fs::create_dir(&raw).unwrap();
        let roots=RootSet::default();let cx=&mut Context::new(&roots);
        root!(env,init(Env::new(cx).unwrap()),cx);
        crate::core::env::init_variables(cx,env);
        env.set_var_context(sym::DEFAULT_DIRECTORY,cx.add(raw.as_os_str().as_bytes().to_vec()),cx).unwrap();
        let copied=super::owner_working_directory(env,cx).unwrap();
        cx.garbage_collect(true);assert_eq!(copied,raw);
        env.set_var_context(sym::DEFAULT_DIRECTORY,cx.add(vec![b'x';65537]),cx).unwrap();assert!(super::owner_working_directory(env,cx).is_err());
        env.set_var_context(sym::DEFAULT_DIRECTORY,cx.add(vec![b'/',0]),cx).unwrap();assert!(super::owner_working_directory(env,cx).is_err());
    }

    #[test]
    fn synchronous_owner_directory_matches_gnu_without_host_chdir() {
        let original=std::env::current_dir().unwrap();
        crate::assert_elprop!(r#"(progn (erase-buffer) (let ((default-directory "/tmp/") (process-environment nil)) (list (call-process "/bin/pwd" nil t) (equal (buffer-substring-no-properties 1 (point-max)) "/tmp\n") default-directory)))"#);
        assert_eq!(std::env::current_dir().unwrap(),original);
    }
    #[test]
    fn invalid_owner_directory_has_no_process_or_output_buffer_side_effect() {
        crate::interpreter::assert_lisp(r#"(let ((default-directory "relative")) (list (condition-case nil (start-process "cwd-no-process" "cwd-no-buffer" "/bin/true") (error 'caught)) (get-process "cwd-no-process") (get-buffer "cwd-no-buffer") (condition-case nil (call-process "/bin/true" nil "cwd-no-sync-buffer") (error 'caught)) (get-buffer "cwd-no-sync-buffer")))"#,"(caught nil nil caught nil)");
        crate::interpreter::assert_lisp(r#"(let ((v (vector nil))) (aset v 0 v) (condition-case e (let ((default-directory v)) (call-process "/bin/true")) (wrong-type-argument (list (car e) (car (cdr e)) (eq v (car (cdr (cdr e))))))))"#, "(wrong-type-argument stringp t)");
    }
    #[test]
    fn asynchronous_owner_directory_reaches_real_child() {
        crate::interpreter::assert_lisp(r#"(progn (erase-buffer) (let ((default-directory "/tmp/") (process-environment nil) (done nil) (p nil) (attempts 0)) (setq p (start-process "cwd-async" (current-buffer) "/bin/pwd")) (set-process-sentinel p #'(lambda (process event) (setq done t))) (unwind-protect (progn (while (and (null done) (< attempts 200)) (setq attempts (+ attempts 1)) (accept-process-output p 0.01)) (list done (process-exit-status p) (buffer-substring-no-properties 1 (point-max)))) (delete-process p))))"#,"(t 0 \"/tmp\\n\")");
    }
}

#[cfg(test)]
mod structured_process_boundary_tests {
    #[test]
    fn cyclic_invalid_process_arguments_retain_identity_without_formatting() {
        for graph in ["(vector nil)","(list nil)","(make-byte-code 0 (make-string 0 0) (vector nil) 0)"] {
            let mutate=if graph.starts_with("(list") {"(setcar v v)"} else if graph.starts_with("(vector") {"(aset v 0 v)"} else {"(aset (aref v 2) 0 v)"};
            for (call,predicate) in [
                ("(get-process v)","stringp"),("(process-status v)","processp"),
                ("(process-name v)","processp"),("(process-send-string v v)","stringp"),
                ("(process-send-string v \"x\")","processp"),
                ("(internal-default-process-filter v v)","processp"),
                ("(internal-default-process-sentinel v v)","processp"),
                ("(start-process \"x\" v 42)","stringp"),
                ("(start-process \"x\" nil v)","stringp"),
                ("(start-process \"x\" nil \"/bin/true\" v)","stringp"),
                ("(call-process v nil nil nil v)","stringp"),
                ("(call-process \"/bin/true\" v nil nil v)","stringp"),
                ("(get-buffer-process v)","stringp"),
            ] {
                let source=format!("(let ((v {})) {} (condition-case e {} (wrong-type-argument (list (car e) (car (cdr e)) (eq v (car (cdr (cdr e))))))))",graph,mutate,call);
                crate::interpreter::assert_lisp(&source,&format!("(wrong-type-argument {predicate} t)"));
            }
            crate::interpreter::assert_lisp(&format!("(let ((v {})) {} (condition-case e (start-process v nil 42) (error e)))",graph,mutate),"(error \":name value not a string\")");
        }
    }
    #[test]
    fn process_type_order_matches_gnu_probed_controls() {
        for (form,result) in [
            ("(get-process 42)","(wrong-type-argument stringp 42)"),
            ("(process-send-string 42 17)","(wrong-type-argument stringp 17)"),
            ("(start-process 42 43 44)","(wrong-type-argument stringp 43)"),
            ("(start-process 42 nil 44)","(error \":name value not a string\")"),
            ("(start-process \"x\" nil 17 44)","(wrong-type-argument stringp 17)"),
            ("(call-process 42 nil 43 nil 44)","(wrong-type-argument stringp 42)"),
            ("(call-process \"/bin/true\" nil 43 nil 44)","(wrong-type-argument stringp 44)"),
        ] { crate::interpreter::assert_lisp(&format!("(condition-case e {form} (error e))"),result); }
    }
    #[test]
    fn arbitrary_callback_values_are_rooted_without_eager_function_conversion() {
        crate::interpreter::assert_lisp(r#"(let ((p (start-process "callback-boundary" nil "/bin/cat")) (v (vector nil))) (aset v 0 v) (unwind-protect (progn (set-process-filter p v) (set-process-sentinel p v) (garbage-collect) (list (eq v (process-filter p)) (eq v (process-sentinel p)) (eq 42 (set-process-filter p 42)) (eq 42 (set-process-sentinel p 42)))) (set-process-filter p nil) (set-process-sentinel p nil) (delete-process p)))"#,"(t t t t)");
    }
    #[test]
    fn invalid_callback_delivery_signals_original_graph_after_moving_gc() {
        for setter in ["set-process-filter","set-process-sentinel"] {
            let source=format!(r#"(let ((p (start-process "callback-delivery-boundary" nil "/usr/bin/python3" "-c" "print('x')")) (v (vector nil)) (observed nil) (n 0)) (aset v 0 v) ({} p v) (garbage-collect) (unwind-protect (progn (while (and (null observed) (< n 200)) (setq n (+ n 1)) (setq observed (condition-case e (progn (accept-process-output p 0.01) nil) (invalid-function (eq v (car (cdr e))))))) (list observed (processp p))) (set-process-filter p nil) (set-process-sentinel p nil) (delete-process p)))"#,setter);
            crate::interpreter::assert_lisp(&source,"(t t)");
        }
    }

    #[test]
    fn native_argument_budgets_reject_before_copy_or_spawn() {
        use crate::core::gc::{Context,RootSet};
        let roots=RootSet::default();let cx=Context::new(&roots);
        let large=cx.add(vec![b'x';super::MAX_ARGUMENT_BYTES+1]);
        assert!(super::native_string(large,super::MAX_ARGUMENT_BYTES,&cx).is_err());
        let one=cx.add(vec![b'x';super::MAX_ARGUMENT_BYTES]);
        assert!(super::arguments(&vec![one;17],&cx).is_err());
        assert!(super::arguments(&vec![one;4097],&cx).is_err());
        assert!(super::native_string(cx.add(vec![0]),super::MAX_ARGUMENT_BYTES,&cx).is_err());
    }
}
