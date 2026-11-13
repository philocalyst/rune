//! Opt-in official batch CLI oracle. No server, socket, or shell transport.
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_FORM: usize = 64 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
static REQUEST: AtomicUsize = AtomicUsize::new(0);

pub(super) fn eval(form: &str) -> Result<String> {
    ensure!(form.len() <= MAX_FORM, "batch oracle form exceeds size limit");
    let nonce =
        format!("RUNE-BATCH-{}-{}", std::process::id(), REQUEST.fetch_add(1, Ordering::Relaxed));
    let driver = driver(form, &nonce);
    let mut command = Command::new("emacs");
    command.args(["-Q", "--batch", "--eval", &driver]);
    let output = capture(command, TIMEOUT, MAX_OUTPUT)?;
    parse(&output, &nonce)
}

fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\000"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}
fn driver(form: &str, nonce: &str) -> String {
    // A second read must reach EOF. EOF can also mean unfinished reader
    // syntax (including #1=), so independently restrict its tail to ordinary
    // whitespace and semicolon comments. No sentinel can serve as syntax data.
    let evaluation = format!(
        "(let* ((input {}) (parsed (read-from-string input)) (tail (substring input (cdr parsed)))) (condition-case nil (progn (read-from-string tail) (error \"GNU batch oracle requires exactly one form\")) (end-of-file (let ((i 0) (n (length tail))) (while (< i n) (cond ((memq (aref tail i) '(9 10 11 12 13 32)) (setq i (1+ i))) ((= (aref tail i) 59) (while (and (< i n) (/= (aref tail i) 10)) (setq i (1+ i)))) (t (error \"GNU batch oracle has non-comment trailing syntax\"))))))) (eval (car parsed)))",
        literal(form)
    );
    // Redirect incidental princ output while evaluating; diagnostics remain on
    // stderr. Frame delimiters disable escaping so their newlines remain real.
    format!(
        "(let ((print-length nil) (print-level nil) (print-circle t) (print-escape-newlines t)) (condition-case err (let ((value (with-temp-buffer (let ((standard-output (current-buffer))) {})))) (let ((print-escape-newlines nil)) (princ {}))(prin1 value)(let ((print-escape-newlines nil)) (princ {}))) (error (let ((print-escape-newlines nil)) (princ {}))(prin1 err)(let ((print-escape-newlines nil)) (princ {})))))",
        evaluation,
        literal(&format!("{nonce}:VALUE\n")),
        literal(&format!("\n{nonce}:END\n")),
        literal(&format!("{nonce}:ERROR\n")),
        literal(&format!("\n{nonce}:END\n"))
    )
}
fn parse(output: &str, nonce: &str) -> Result<String> {
    let end = format!("\n{nonce}:END\n");
    let value = format!("{nonce}:VALUE\n");
    let error = format!("{nonce}:ERROR\n");
    if let Some(body) = output.strip_prefix(&error).and_then(|s| s.strip_suffix(&end)) {
        bail!("GNU batch oracle signaled a Lisp condition: {body}");
    }
    let body = output
        .strip_prefix(&value)
        .and_then(|s| s.strip_suffix(&end))
        .context("invalid or incomplete GNU batch oracle framing")?;
    ensure!(
        !body.is_empty()
            && !body.starts_with(&value)
            && !body.starts_with(&error)
            && !body.contains(&format!("\n{nonce}:")),
        "duplicate or empty GNU batch oracle framing"
    );
    Ok(body.to_owned())
}

fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    // SAFETY: The child was spawned in its own process group; the negative
    // PID addresses only that oracle job, including descendants retaining pipes.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn capture(mut command: Command, timeout: Duration, limit: usize) -> Result<String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().context("cannot launch GNU Emacs batch oracle")?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel();
    for (is_stdout, mut pipe) in [
        (true, Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>),
        (false, Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>),
    ] {
        let sender = sender.clone();
        let count = count.clone();
        let exceeded = exceeded.clone();
        thread::spawn(move || {
            let mut output = Vec::new();
            let mut chunk = [0; 4096];
            let result = loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break Ok(output),
                    Ok(n) => {
                        let before = count.fetch_add(n, Ordering::Relaxed);
                        let keep = n.min(limit.saturating_sub(before));
                        output.extend_from_slice(&chunk[..keep]);
                        if keep != n {
                            exceeded.store(true, Ordering::Release);
                            break Ok(output);
                        }
                    }
                    Err(e) => break Err(e),
                }
            };
            let _ = sender.send((is_stdout, result));
        });
    }
    drop(sender);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if exceeded.load(Ordering::Acquire) || Instant::now() >= deadline {
            stop(&mut child);
            bail!("GNU batch oracle exceeded time or output limit");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                stop(&mut child);
                return Err(error.into());
            }
        }
        thread::sleep(Duration::from_millis(5));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for _ in 0..2 {
        let (is_stdout, bytes) = match receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(output) => output,
            Err(error) => {
                stop(&mut child);
                return Err(error).context("GNU batch oracle output did not close before deadline");
            }
        };
        if is_stdout {
            stdout = bytes?;
        } else {
            stderr = bytes?;
        }
    }
    ensure!(!exceeded.load(Ordering::Acquire), "GNU batch oracle exceeded output limit");
    let stderr = String::from_utf8(stderr).context("GNU batch oracle stderr is not UTF-8")?;
    ensure!(status.success(), "GNU batch oracle exited with {status}: {stderr}");
    String::from_utf8(stdout).context("GNU batch oracle stdout is not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_is_exact_and_conditions_are_not_values() {
        assert_eq!(parse("n:VALUE\n\"λ\\n🙂\"\nn:END\n", "n").unwrap(), "\"λ\\n🙂\"");
        for bad in [
            "42",
            "n:VALUE\n42",
            "noise\nn:VALUE\n42\nn:END\n",
            "n:VALUE\nn:VALUE\n42\nn:END\n",
            "n:ERROR\n(error \"bad\")\nn:END\n",
        ] {
            assert!(parse(bad, "n").is_err());
        }
    }
    #[test]
    fn forms_are_quoted_not_driver_syntax() {
        assert_eq!(literal("\"\\\n\0"), "\"\\\"\\\\\\n\\000\"");
        assert!(driver("\") (kill-emacs 42) (\"", "nonce").contains("read-from-string"));
    }
    #[test]
    #[ignore = "requires GNU Emacs on PATH; uses its official batch CLI"]
    fn gnu_single_form_reader_accepts_comments_rejects_extra_or_incomplete_forms() {
        for form in ["42", "42 \n ; trailing comment", "42 ; comment at EOF"] {
            assert_eq!(eval(form).unwrap(), "42");
        }
        for form in [
            "42 #1=",
            "42 #1=\"",
            "42 43",
            "42 garbage",
            "42 (",
            "42 \"",
            "42 '",
            "42 )",
            "42 ; comment\n43",
        ] {
            assert!(eval(form).is_err(), "accepted trailing input: {form:?}");
        }
    }

    #[test]
    fn single_form_driver_checks_eof_and_incomplete_tail() {
        let source = driver("42 ; trailing comment", "frame");
        assert!(source.contains("(read-from-string tail)"));
        assert!(source.contains("(memq (aref tail i)"));
        assert!(source.contains("(eval (car parsed))"));
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_timeout_output_limit_and_status_are_failures() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2"]);
        assert!(capture(command, Duration::from_millis(30), 100).is_err());
        let mut command = Command::new("sh");
        command.args(["-c", "printf 123456789"]);
        assert!(capture(command, Duration::from_secs(2), 4).is_err());
        let mut command = Command::new("sh");
        command.args(["-c", "printf bad >&2; exit 7"]);
        assert!(capture(command, Duration::from_secs(2), 100).is_err());
    }
}
