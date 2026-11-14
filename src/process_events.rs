//! Owner-thread delivery of native process events. Workers never evaluate Lisp.
use crate::{
    core::{
        env::{CallFrame, Env},
        gc::{Context, Rt, Rto},
        object::{
            Function, Gc, LispProcess, NIL, Number, Object, ObjectType, ProcessCallbackSnapshot,
            ProcessCoding,
        },
    },
    process_transport::{OutputStream, ProcessEvent},
};
use anyhow::{Result, bail, ensure};
use rune_core::macros::root;
use rune_macros::defun;
use std::time::{Duration, Instant};

fn invoke(
    snapshot: &ProcessCallbackSnapshot,
    sentinel: bool,
    argument: &Rto<Object>,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<()> {
    let process = snapshot.process.bind(cx)?;
    root!(process, cx);
    let typed: Gc<&LispProcess> = process.bind(cx).try_into()?;
    let current = typed.callback_snapshot(cx)?;
    let callback = if sentinel { current.sentinel.bind(cx)? } else { current.filter.bind(cx)? };
    if callback.is_nil() {
        if sentinel {
            let text = argument.bind(cx);
            let ObjectType::String(text) = text.untag() else {
                bail!("invalid native sentinel event")
            };
            crate::process::internal_default_process_sentinel(process.bind(cx), text, cx)?;
        } else {
            crate::process::internal_default_process_filter(
                process.bind(cx),
                argument.bind(cx),
                cx,
            )?;
        }
        return Ok(());
    }
    let callback: Function = callback.try_into()?;
    root!(callback, cx);
    let mut frame = CallFrame::new(env);
    frame.push_arg(process.bind(cx));
    frame.push_arg(argument.bind(cx));
    callback.call(&mut frame, Some("process callback"), cx)?;
    Ok(())
}

/// Decode complete UTF8 prefixes only. A trailing partial sequence stays owned
/// by the Context record; invalid complete bytes are a real delivery error.
fn decode(bytes: Vec<u8>, pending: &mut Vec<u8>) -> Result<String> {
    pending.extend(bytes);
    let valid = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => {
            pending.clear();
            bail!("Invalid UTF8 subprocess output");
        }
    };
    let text = std::str::from_utf8(&pending[..valid])?.to_owned();
    pending.drain(..valid);
    Ok(text)
}

fn fail_closed_stream(process: &LispProcess, cx: &Context) -> Result<()> {
    let transport = process.transport(cx)?;
    cx.update_process_state(process, |state| {
        state.pending_utf8.clear();
        state.stderr_pending_utf8.clear();
        // A failed decoder retires delivery, not the retained process metadata.
        state.sentinel_delivered = true;
    })?;
    transport.terminate();
    Ok(())
}
fn stderr_diagnostic(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    for byte in bytes.iter().take(4096) {
        if (32..=126).contains(byte) {
            text.push(char::from(*byte));
        } else {
            write!(&mut text, "\\x{byte:02X}").expect("String formatting cannot fail");
        }
    }
    if bytes.len() > 4096 {
        text.push_str(" [truncated]");
    }
    text
}

fn deliver(
    snapshot: &ProcessCallbackSnapshot,
    event: ProcessEvent,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<bool> {
    let process: Gc<&LispProcess> = snapshot.process.bind(cx)?.try_into()?;
    root!(process, cx);
    let mut output = false;
    match event {
        ProcessEvent::Output { stream: OutputStream::Stdout, bytes } => {
            output = !bytes.is_empty();
            let state = cx.process_state(&process.bind(cx))?;
            let argument = if state.read_coding == ProcessCoding::Binary {
                cx.add(bytes)
            } else {
                let decoded = cx.update_process_state(&process.bind(cx), |state| {
                    decode(bytes, &mut state.pending_utf8)
                })?;
                let text = match decoded {
                    Ok(text) => text,
                    Err(error) => {
                        fail_closed_stream(&process.bind(cx), cx)?;
                        return Err(error);
                    }
                };
                if text.is_empty() {
                    return Ok(output);
                }
                cx.add(text)
            };
            // Snapshot callback ownership survives all moving collections in call.
            root!(argument, cx);
            invoke(snapshot, false, argument, env, cx)?;
        }
        ProcessEvent::Output { stream: OutputStream::Stderr, bytes } => {
            // Separate diagnostic pipe MUST NOT corrupt a binary RPC stdout filter.
            output = !bytes.is_empty();
            eprintln!("Process stderr: {}", stderr_diagnostic(&bytes));
        }
        ProcessEvent::Eof(stream) => {
            cx.update_process_state(&process.bind(cx), |state| match stream {
                OutputStream::Stdout => state.stdout_eof = true,
                OutputStream::Stderr => state.stderr_eof = true,
            })?;
        }
        ProcessEvent::Exit(exit) => {
            cx.update_process_state(&process.bind(cx), |state| state.pending_exit = Some(exit))?;
        }
        ProcessEvent::Failure(_) => bail!("Native process I/O failure"),
        ProcessEvent::LimitReached => bail!("Native process output queue limit reached"),
    }
    let completion = cx.update_process_state(&process.bind(cx), |state| {
        if state.stdout_eof && state.stderr_eof && !state.sentinel_delivered {
            if let Some(exit) = state.pending_exit {
                state.sentinel_delivered = true;
                return Some((
                    exit,
                    !state.pending_utf8.is_empty() || !state.stderr_pending_utf8.is_empty(),
                ));
            }
        }
        None
    })?;
    if let Some((exit, incomplete)) = completion {
        // Delivery is committed BEFORE any user callback, including recursive
        // accept-process-output or a callback that throws/signals/quits.
        if incomplete {
            fail_closed_stream(&process.bind(cx), cx)?;
            bail!("Incomplete UTF8 sequence at process EOF");
        }
        let event = if let Some(signal) = exit.signal {
            format!("killed by signal {signal}\n")
        } else if exit.code == Some(0) {
            "finished\n".into()
        } else {
            format!("exited abnormally with code {}\n", exit.code.unwrap_or(0))
        };
        let argument = cx.add(event);
        root!(argument, cx);
        invoke(snapshot, true, argument, env, cx)?;
        // Keep completed metadata for retained process objects. Strong registry
        // records are currently retired only at Context teardown (known limit).
    }
    Ok(output)
}

pub(crate) fn pump(
    snapshots: Vec<ProcessCallbackSnapshot>,
    timeout: Duration,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut events = 0usize;
    loop {
        env.poll_quit(cx)?;
        let mut progress = false;
        let mut output = false;
        let mut active = false;
        for snapshot in &snapshots {
            let process: Gc<&LispProcess> = snapshot.process.bind(cx)?.try_into()?;
            if cx.process_state(&process)?.sentinel_delivered {
                continue;
            }
            active = true;
            ensure!(events < 1024, "Process event budget exceeded");
            if let Some(event) = snapshot.transport.recv(Duration::ZERO) {
                events += 1;
                progress = true;
                output |= deliver(snapshot, event, env, cx)?;
            }
        }
        if output {
            return Ok(true);
        }
        if !active || Instant::now() >= deadline {
            return Ok(false);
        }
        if !progress {
            std::thread::sleep(
                Duration::from_millis(2).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

/// Nonblocking owner-turn hook. Drain currently queued events, including EOF
/// and exit bookkeeping, even when no stdout callback produced text.
pub(crate) fn poll_process_events(env: &mut Rt<Env>, cx: &mut Context) -> Result<bool> {
    let mut output = false;
    let mut events = 0usize;
    loop {
        env.poll_quit(cx)?;
        let mut progress = false;
        for snapshot in cx.process_snapshots() {
            let process: Gc<&LispProcess> = snapshot.process.bind(cx)?.try_into()?;
            if cx.process_state(&process)?.sentinel_delivered {
                continue;
            }
            ensure!(events < 1024, "Process owner-turn event budget exceeded");
            if let Some(event) = snapshot.transport.recv(Duration::ZERO) {
                events += 1;
                progress = true;
                output |= deliver(&snapshot, event, env, cx)?;
            }
        }
        if !progress {
            return Ok(output);
        }
    }
}

/// Initial bounded pipe subset: no timers, sockets, ptys, or synchronous UI work.
#[defun]
fn accept_process_output(
    process: Option<&Rto<Object>>,
    seconds: Option<&Rto<Object>>,
    millisec: Option<i64>,
    _just_this_one: Option<&Rto<Object>>,
    env: &mut Rt<Env>,
    cx: &mut Context,
) -> Result<bool> {
    let seconds = match seconds.map(|s| s.bind(cx)) {
        None => {
            if millisec.is_some() {
                0.0
            } else {
                10.0
            }
        }
        Some(s) if s.is_nil() => {
            if millisec.is_some() {
                0.0
            } else {
                10.0
            }
        }
        Some(s) => Number::try_from(s)?.to_float(),
    };
    let seconds = seconds + millisec.unwrap_or(0) as f64 / 1000.0;
    ensure!(seconds.is_finite(), "Process wait must be finite");
    let seconds = seconds.max(0.0);
    ensure!(
        seconds.is_finite() && (0.0..=10.0).contains(&seconds),
        "Process wait must be between zero and ten seconds"
    );
    let snapshots = match process.map(|p| p.bind(cx)) {
        None => cx.process_snapshots(),
        Some(object) if object.is_nil() => cx.process_snapshots(),
        Some(object) => {
            let object = match object.untag() {
                ObjectType::String(name) => cx
                    .find_process(name)
                    .ok_or_else(|| anyhow::anyhow!("No process with requested name"))?
                    .bind(cx)?,
                _ => object,
            };
            let process: Gc<&LispProcess> = object.try_into()?;
            vec![process.callback_snapshot(cx)?]
        }
    };
    pump(snapshots, Duration::from_secs_f64(seconds), env, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::assert_lisp;
    #[test]
    fn utf8_chunk_carry_is_exact_and_invalid_bytes_are_not_lossy() {
        let mut pending = Vec::new();
        assert_eq!(decode(vec![b'a', 0xc3], &mut pending).unwrap(), "a");
        assert_eq!(pending, [0xc3]);
        assert_eq!(decode(vec![0xa9, b'b'], &mut pending).unwrap(), "éb");
        assert!(pending.is_empty());
        assert!(decode(vec![0xff], &mut pending).is_err());
        assert!(pending.is_empty());
        assert_eq!(
            stderr_diagnostic(&[27, b'[', b'3', b'1', b'm', 13, 10, 255]),
            "\\x1B[31m\\x0D\\x0A\\xFF"
        );
    }
    #[test]
    fn real_child_filters_gc_and_sentinel_run_on_owner_exactly_once() {
        assert_lisp(
            r#"(progn
          (setq pump-text "" pump-done nil pump-calls 0)
          (let ((p (start-process "rune-owner-pump" nil "/usr/bin/python3" "-c" "import os; os.write(1,'hé'.encode())")) (n 0))
            (set-process-coding-system p 'utf-8-unix 'utf-8-unix)
            (set-process-filter p #'(lambda (process text) (setq pump-text (concat pump-text text)) (garbage-collect)))
            (set-process-sentinel p #'(lambda (process event) (setq pump-calls (+ pump-calls 1) pump-done event) (garbage-collect)))
            (while (and (null pump-done) (< n 200)) (setq n (+ n 1)) (accept-process-output p 0.05))
            (accept-process-output p 0)
            (list pump-text pump-done pump-calls (processp p) (stringp (process-name p)))))"#,
            "(\"hé\" \"finished\\n\" 1 t t)",
        );
    }
    #[test]
    fn filter_replacement_and_recursive_poll_use_current_metadata_without_replay() {
        assert_lisp(
            r#"(progn
          (setq pump-old 0 pump-new 0 pump-finished nil)
          (let ((p (start-process "rune-reentrant-pump" nil "/usr/bin/python3" "-c" "import os,sys; os.write(1,b'one'); sys.stdin.buffer.read(1); os.write(1,b'two')")) (n 0))
            (set-process-coding-system p 'utf-8-unix 'utf-8-unix)
            (set-process-filter p #'(lambda (process text)
              (setq pump-old (+ pump-old 1))
              (set-process-filter process #'(lambda (process text) (setq pump-new (+ pump-new 1)) (garbage-collect)))
              (process-send-string process "a")
              (accept-process-output process 0)))
            (set-process-sentinel p #'(lambda (process event) (setq pump-finished t) (accept-process-output process 0)))
            (while (and (null pump-finished) (< n 200)) (setq n (+ n 1)) (accept-process-output p 0.05))
            (list pump-old pump-new pump-finished)))"#,
            "(1 1 t)",
        );
    }
    #[test]
    fn binary_output_is_not_unicode_reencoded() {
        assert_lisp(
            r#"(progn
          (setq pump-bytes nil pump-binary-done nil)
          (let ((p (start-process "rune-binary-pump" nil "/usr/bin/python3" "-c" "import os; os.write(1,bytes([254,255]))")) (n 0))
            (set-process-coding-system p 'binary 'binary)
            (set-process-filter p #'(lambda (process text) (setq pump-bytes (list (aref text 0) (aref text 1))) (garbage-collect)))
            (set-process-sentinel p #'(lambda (process event) (setq pump-binary-done t)))
            (while (and (null pump-binary-done) (< n 200)) (setq n (+ n 1)) (accept-process-output p 0.05))
            (list pump-bytes pump-binary-done)))"#,
            "((254 255) t)",
        );
    }
    #[test]
    fn invalid_utf8_retires_delivery_and_terminates_without_poisoned_carry_growth() {
        assert_lisp(
            r#"(progn
          (setq pump-invalid nil pump-invalid-calls 0)
          (let ((p (start-process "rune-invalid-utf8" nil "/usr/bin/python3" "-c" "import os,time; os.write(1,bytes([255])); time.sleep(30)")) (n 0))
            (set-process-coding-system p 'utf-8-unix 'utf-8-unix)
            (set-process-filter p #'(lambda (process text) (setq pump-invalid-calls (+ pump-invalid-calls 1))))
            (while (and (null pump-invalid) (< n 200)) (setq n (+ n 1))
              (condition-case nil (accept-process-output p 0.05) (error (setq pump-invalid t))))
            (accept-process-output p 0)
            (list pump-invalid pump-invalid-calls (null (get-process "rune-invalid-utf8")) (process-name p))))"#,
            "(t 0 t \"rune-invalid-utf8\")",
        );
    }
    #[test]
    fn callback_signal_consumes_event_and_keeps_registry_metadata() {
        assert_lisp(
            r#"(progn
          (setq pump-signals 0)
          (let ((p (start-process "rune-signalling-sentinel" nil "/usr/bin/python3" "-c" "pass")) (n 0))
            (set-process-sentinel p #'(lambda (process event) (setq pump-signals (+ pump-signals 1)) (error "sentinel failure")))
            (while (and (= pump-signals 0) (< n 200)) (setq n (+ n 1)) (condition-case nil (accept-process-output p 0.05) (error nil)))
            (accept-process-output p 0)
            (list pump-signals (process-name p) (functionp (process-sentinel p))))))"#,
            "(1 \"rune-signalling-sentinel\" t)",
        );
    }
}
