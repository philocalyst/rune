#![allow(unsafe_op_in_unsafe_fn)]
#[macro_use]
mod macros;
#[macro_use]
mod core;
#[macro_use]
mod debug;
mod alloc;
mod arith;
mod buffer;
mod bytecode;
mod casefiddle;
mod channel_manager;
mod channels;
mod character;
mod chartab;
mod data;
mod dired;
mod dump;
mod editfns;
mod editor_document;
mod emacs;
mod eval;
mod fileio;
mod filelock;
mod floatfns;
mod fns;
mod interpreter;
mod intervals;
mod keymap;
mod library;
mod lisp;
mod lread;
#[cfg(feature = "native")]
mod native;
mod print;
mod reader;
mod search;
mod textprops;
mod threads;
mod timefns;

use crate::core::{
    env::{Env, intern, sym},
    gc::{Context, RootSet, Rt},
    object::{Gc, LispString, NIL},
};
use crate::eval::EvalError;
use clap::Parser;
use rune_core::macros::root;
use std::io::{self, Write};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, value_name = "FILE")]
    load: Vec<String>,
    #[arg(short, long)]
    repl: bool,
    #[arg(short, long)]
    no_bootstrap: bool,
    #[arg(long)]
    eval_stdin: bool,
    #[arg(long)]
    dump: bool,
    #[arg(long, value_name = "FILE")]
    load_dump: Option<String>,
    /// Launch the opt-in native editor with a minimal, unbootstrapped runtime.
    #[arg(long, conflicts_with_all = ["load", "repl", "eval_stdin", "dump", "load_dump"])]
    native: bool,
    /// Evaluate startup source on the native owner, in input order.
    #[arg(long, requires = "native", value_name = "SOURCE")]
    native_eval: Vec<String>,
}

fn main() -> Result<(), ()> {
    let args = Args::parse();
    if args.native {
        if args.repl
            || args.eval_stdin
            || args.dump
            || args.load_dump.is_some()
            || !args.load.is_empty()
        {
            eprintln!(
                "--native cannot be combined with --repl, --eval-stdin, --dump, --load-dump or --load"
            );
            return Err(());
        }
        #[cfg(feature = "native")]
        return native::run(args.native_eval);
        #[cfg(not(feature = "native"))]
        {
            eprintln!("--native requires building with --features native");
            return Err(());
        }
    }

    let roots = &RootSet::default();
    let cx = &mut Context::new(roots);
    root!(
        env,
        init(Env::new(cx).map_err(|error| eprintln!("Cannot initialize editor: {error}"))?),
        cx
    );

    sym::init_symbols();
    crate::core::env::init_variables(cx, env);
    crate::data::defalias(intern("not", cx), (sym::NULL).into(), None)
        .expect("null should be defined");

    if args.eval_stdin {
        return eval_stdin(cx, env);
    }

    // Load from heap dump if specified
    if let Some(ref dump_path) = args.load_dump {
        if args.dump {
            eprintln!("error: cannot provide both --dump and --load-dump");
            return Err(());
        }

        let path = std::path::Path::new(dump_path);
        eprintln!("Loading dump from {}", path.display());
        dump::load_dump(path, env, cx).map_err(|e| eprintln!("Load failed: {e}"))?;
        eprintln!("Load complete");
    } else if !args.no_bootstrap {
        bootstrap(env, cx)?;
    }

    if args.dump {
        let path = std::path::Path::new("rune.pdmp");
        eprintln!("Dumping heap to {}", path.display());
        dump::dump_to_file(path, env, cx).map_err(|e| eprintln!("Dump failed: {e}"))?;
        eprintln!("Dump complete");
    }

    for file in args.load {
        load_command_line_file(&file, cx, env)?;
    }

    if args.repl {
        repl(env, cx);
    }
    Ok(())
}

fn parens_closed(buffer: &str) -> bool {
    let open = buffer.chars().filter(|&x| x == '(').count();
    let close = buffer.chars().filter(|&x| x == ')').count();
    open <= close
}

fn repl(env: &mut Rt<Env>, cx: &mut Context) {
    repl_with_input(io::stdin().lock(), env, cx);
}

fn repl_with_input(mut input: impl io::BufRead, env: &mut Rt<Env>, cx: &mut Context) {
    let mut buffer = String::new();
    loop {
        print!("> ");
        io::stdout().flush().unwrap();
        if input.read_line(&mut buffer).unwrap() == 0 {
            return;
        }
        if buffer.trim() == "exit" {
            return;
        }
        if buffer.trim().is_empty() {
            continue;
        }
        if !parens_closed(&buffer) {
            continue;
        }
        let (obj, _) = match reader::read(&buffer, cx) {
            Ok(obj) => obj,
            Err(e) => {
                eprintln!("Error: {e}");
                buffer.clear();
                continue;
            }
        };

        root!(obj, cx);
        match interpreter::eval(obj, None, env, cx) {
            Ok(val) => println!("{val}"),
            Err(e) => {
                eprintln!("Error: {e}");
                if let Ok(e) = e.downcast::<EvalError>() {
                    e.print_backtrace();
                }
            }
        }
        buffer.clear();
    }
}

/// GNU startup.el's -load handler prefers an existing regular file in the
/// invocation directory, but leaves library names for load-path resolution.
fn command_line_load_name(file: &str) -> io::Result<String> {
    if !std::path::Path::new(file).is_file() {
        return Ok(file.to_owned());
    }
    std::fs::canonicalize(file)?.into_os_string().into_string().map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "load file path is not valid UTF-8")
    })
}

fn load_command_line_file(file: &str, cx: &mut Context, env: &mut Rt<Env>) -> Result<(), ()> {
    let file = command_line_load_name(file).map_err(|error| eprintln!("Error: {error}"))?;
    load(&file, cx, env)
}

fn load(file: &str, cx: &mut Context, env: &mut Rt<Env>) -> Result<(), ()> {
    let file: Gc<&LispString> = cx.add_as(file);
    root!(file, cx);
    match crate::lread::load(file, None, None, cx, env) {
        Ok(val) => {
            println!("{val}");
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {e}");
            if let Ok(e) = e.downcast::<EvalError>() {
                e.print_backtrace();
            }
            Err(())
        }
    }
}

fn eval_stdin(cx: &mut Context, env: &mut Rt<Env>) -> Result<(), ()> {
    eval_stdin_with_input(io::stdin().lock(), cx, env)
}

fn eval_stdin_with_input(
    mut input: impl io::BufRead,
    cx: &mut Context,
    env: &mut Rt<Env>,
) -> Result<(), ()> {
    let mut buffer = String::new();
    let mut point = 0;
    let mut count = 0;
    let mut eof = false;
    loop {
        if !eof {
            eof = input.read_line(&mut buffer).unwrap() == 0;
        }
        let obj = match reader::read(&buffer[point..], cx) {
            Ok((obj, offset)) => {
                point += offset;
                obj
            }
            // Drain forms already buffered on the final line before finishing.
            Err(reader::Error::EmptyStream) if eof => return Ok(()),
            Err(reader::Error::EmptyStream) => continue,
            Err(e) => {
                eprintln!("Error: {e}");
                break;
            }
        };

        root!(obj, cx);
        match interpreter::eval(obj, None, env, cx) {
            Ok(val) => println!("{val}\n"),
            Err(e) => println!("Error: {e}\n"),
        }
        count += 1;
        std::thread::sleep(std::time::Duration::from_millis(10));
        // timeout after ~1 minute
        if count > 6000 {
            break;
        }
    }
    Err(())
}

fn bootstrap(env: &mut Rt<Env>, cx: &mut Context) -> Result<(), ()> {
    buffer::get_buffer_create(cx.add("*scratch*"), Some(NIL), cx).unwrap();
    load("bootstrap.el", cx, env)
}

#[test]
fn verify_cli() {
    use clap::CommandFactory;
    Args::command().debug_assert()
}

#[cfg(test)]
mod stdin_tests {
    use super::*;
    use std::io::{BufRead, Cursor, Read};

    // Repeated EOF reads fail immediately instead of hanging the test runner.
    struct EofOnce<R> {
        input: R,
        eof_seen: bool,
    }

    impl<R: Read> Read for EofOnce<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl<R: BufRead> BufRead for EofOnce<R> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            self.input.fill_buf()
        }

        fn consume(&mut self, amount: usize) {
            self.input.consume(amount);
        }

        fn read_line(&mut self, buffer: &mut String) -> io::Result<usize> {
            assert!(!self.eof_seen, "stdin loop read EOF more than once");
            let bytes = self.input.read_line(buffer)?;
            self.eof_seen = bytes == 0;
            Ok(bytes)
        }
    }

    fn input(text: &str) -> EofOnce<Cursor<&[u8]>> {
        EofOnce { input: Cursor::new(text.as_bytes()), eof_seen: false }
    }

    #[test]
    fn repl_stops_at_eof_even_with_an_unfinished_form() {
        for text in ["", "\n", "("] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            root!(env, init(Env::new(cx).unwrap()), cx);
            repl_with_input(input(text), env, cx);
        }
    }

    #[test]
    fn eval_stdin_finishes_cleanly_on_empty_or_comment_only_input() {
        for text in ["", " \n", "; comment without a final newline"] {
            let roots = &RootSet::default();
            let cx = &mut Context::new(roots);
            sym::init_symbols();
            root!(env, init(Env::new(cx).unwrap()), cx);
            assert_eq!(eval_stdin_with_input(input(text), cx, env), Ok(()));
        }
    }

    #[test]
    fn eval_stdin_drains_all_forms_buffered_before_eof() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        assert_eq!(
            eval_stdin_with_input(
                input("(setq eof-test-value 1) (setq eof-test-value 2)"),
                cx,
                env
            ),
            Ok(())
        );
        let form = reader::read("eof-test-value", cx).unwrap().0;
        root!(form, cx);
        assert_eq!(interpreter::eval(form, None, env, cx).unwrap().to_string(), "2");
    }

    #[test]
    fn eval_stdin_keeps_malformed_input_as_an_error() {
        let roots = &RootSet::default();
        let cx = &mut Context::new(roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        assert_eq!(eval_stdin_with_input(input("("), cx, env), Err(()));
    }
}

#[cfg(test)]
mod command_line_load_tests {
    use super::*;

    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn relative_command_line_file_loads_outside_load_path() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let relative = format!(
            "rune-cli-load-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let fixture = Fixture(std::env::current_dir().unwrap().join(&relative));
        std::fs::create_dir(&fixture.0).unwrap();
        let file = std::path::Path::new(&relative).join("relative file.el");
        std::fs::write(&file, "(setq cli-relative-load-value 42)").unwrap();
        assert_eq!(
            std::path::PathBuf::from(command_line_load_name(file.to_str().unwrap()).unwrap()),
            std::fs::canonicalize(&file).unwrap()
        );
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new(cx).unwrap()), cx);
        // A nonempty unrelated path must not intercept explicit CLI files.
        env.vars.insert(
            sym::LOAD_PATH,
            rune_core::macros::list!("/nonexistent/rune-cli-load-path"; cx),
        );
        load_command_line_file(file.to_str().unwrap(), cx, env).unwrap();
        let variable = intern("cli-relative-load-value", cx);
        let value = env.vars.get(variable).unwrap().bind(cx);
        assert_eq!(value, 42_i64);
        // Bootstrap/library calls keep the original search contract.
        let library = format!("missing-library-{}", std::process::id());
        assert_eq!(command_line_load_name(&library).unwrap(), library);
        assert_eq!(command_line_load_name(&relative).unwrap(), relative);
    }
}

#[cfg(test)]
mod native_cli_args_tests {
    use super::*;
    #[test]
    fn native_eval_requires_native_and_rejects_other_runtime_modes() {
        assert!(Args::try_parse_from(["rune", "--native-eval", "(insert \"x\")"]).is_err());
        for arguments in [
            vec!["rune", "--native", "--repl"],
            vec!["rune", "--native", "--eval-stdin"],
            vec!["rune", "--native", "--dump"],
            vec!["rune", "--native", "--load", "file.el"],
            vec!["rune", "--native", "--load-dump", "file.pdmp"],
        ] {
            assert!(Args::try_parse_from(arguments).is_err());
        }
        let args = Args::try_parse_from([
            "rune",
            "--native",
            "--native-eval",
            "(setq x 1)",
            "--native-eval",
            "(insert \"λ\")",
        ])
        .unwrap();
        assert!(args.native);
        assert_eq!(args.native_eval, ["(setq x 1)", "(insert \"λ\")"]);
        assert!(Args::try_parse_from(["rune", "--eval-stdin"]).unwrap().eval_stdin);
    }
}
