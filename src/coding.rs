//! Small explicit coding layer for the real mu byte-framed protocol.
//! Invalid UTF8 is rejected, never replaced or mapped byte-for-Unicode.
use crate::core::{
    env::{Env, intern},
    gc::{Context, Rt},
    object::{Object, ObjectType},
};
use anyhow::Result;
use rune_macros::defun;
use rune_core::macros::list;
defvar!(LAST_CODING_SYSTEM_USED);
// GNU30 coding.c's native seed is NIL; original mule-cmds locale setup may
// subsequently select a concrete coding system. Do not invent UTF8 negotiation.
defvar!(LOCALE_CODING_SYSTEM);

const MAX_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodingError {
    Unsupported(String),
    InvalidUtf8 { valid_up_to: usize },
    InvalidAscii,
    DestinationUnsupported,
    SizeLimit,
}
impl std::fmt::Display for CodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(name) => write!(f, "unsupported coding system: {name}"),
            Self::InvalidUtf8 { valid_up_to } => {
                write!(f, "invalid UTF8 coding input at byte {valid_up_to}")
            }
            Self::InvalidAscii => f.write_str("non-ASCII input for ASCII coding"),
            Self::DestinationUnsupported => {
                f.write_str("coding buffer destination is not implemented")
            }
            Self::SizeLimit => f.write_str("coding input/output exceeds 16 MiB limit"),
        }
    }
}
impl std::error::Error for CodingError {}
#[derive(Clone, Copy)]
enum Coding {
    Identity,
    Utf8,
    Unix,
    Dos,
    Mac,
    Binary,
    Ascii,
}
fn codec_type_error(predicate: crate::core::object::Symbol, value: Object, env: &mut Rt<Env>, cx: &Context) -> anyhow::Error {
    env.initialize_condition_metadata(cx);
    let data = list!(predicate,value;cx);
    crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(),data,cx).into()
}
fn validate_string_input(value: Object, env: &mut Rt<Env>, cx: &Context) -> Result<()> {
    if matches!(value.untag(),ObjectType::String(_) | ObjectType::ByteString(_)) { return Ok(()); }
    Err(codec_type_error(crate::sym::STRINGP,value,env,cx))
}
fn select(value: Object, env: &mut Rt<Env>, cx: &Context) -> Result<Coding> {
    if value.is_nil() { return Ok(Coding::Identity); }
    let ObjectType::Symbol(symbol) = value.untag() else {
        return Err(codec_type_error(crate::sym::SYMBOLP,value,env,cx));
    };
    Ok(match symbol.name() {
        "utf-8" => Coding::Utf8,
        "utf-8-unix" => Coding::Unix,
        "utf-8-dos" => Coding::Dos,
        "utf-8-mac" => Coding::Mac,
        "binary" | "no-conversion" => Coding::Binary,
        "us-ascii" | "ascii" => Coding::Ascii,
        other => return Err(CodingError::Unsupported(other.to_owned()).into()),
    })
}
fn input_bytes(value: Object) -> Result<&[u8]> {
    match value.untag() {
        ObjectType::String(string) => Ok(string.as_bytes()),
        ObjectType::ByteString(string) => Ok(&string),
        _ => anyhow::bail!("Coding input is not a String"),
    }
}
fn decoded(bytes: &[u8], coding: Coding) -> Result<(String, &'static str)> {
    if bytes.len() > MAX_BYTES {
        return Err(CodingError::SizeLimit.into());
    }
    if matches!(coding, Coding::Ascii) && !bytes.is_ascii() {
        return Err(CodingError::InvalidAscii.into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| CodingError::InvalidUtf8 { valid_up_to: error.valid_up_to() })?;
    let mode = if matches!(coding, Coding::Utf8) {
        let cr = bytes.iter().filter(|b| **b == b'\r').count();
        let lf = bytes.iter().filter(|b| **b == b'\n').count();
        let crlf = bytes.windows(2).filter(|w| *w == b"\r\n").count();
        if cr > 0 && cr == lf && crlf == lf {
            Coding::Dos
        } else if cr > 0 && lf == 0 {
            Coding::Mac
        } else if cr == 0 && lf == 0 {
            Coding::Utf8
        } else {
            Coding::Unix
        }
    } else {
        coding
    };
    Ok(match mode {
        Coding::Dos => (text.replace("\r\n", "\n"), "utf-8-dos"),
        Coding::Mac => (text.replace('\r', "\n"), "utf-8-mac"),
        Coding::Unix => (text.to_owned(), "utf-8-unix"),
        Coding::Ascii => (text.to_owned(), "us-ascii"),
        _ => (text.to_owned(), "utf-8"),
    })
}
fn remember(env: &mut Rt<Env>, name: &str, cx: &Context) -> Result<()> {
    env.set_var_context(intern("last-coding-system-used", cx), intern(name, cx).into(), cx)
}
fn nil_coding_copy<'ob>(string: Object<'ob>, nocopy: Option<Object>, env: &mut Rt<Env>, cx: &'ob Context) -> Result<Object<'ob>> {
    if !matches!(string.untag(), ObjectType::String(_) | ObjectType::ByteString(_)) {
        env.initialize_condition_metadata(cx);
        let data = rune_core::macros::list!(crate::sym::STRINGP, string; cx);
        return Err(crate::eval::EvalError::signal(crate::sym::WRONG_TYPE_ARGUMENT.into(), data, cx).into());
    }
    if input_bytes(string)?.len() > MAX_BYTES { return Err(CodingError::SizeLimit.into()); }
    remember(env, "no-conversion", cx)?;
    if nocopy.is_some_and(|v| !v.is_nil()) { return Ok(string); }
    // Validated same-owner String, no callback/GC under shared cx; the existing
    // string-copy helper copies plist spines and retains property-value identity.
    crate::fns::copy_sequence(string, cx)
}

#[defun]
fn decode_coding_string<'ob>(
    string: Object<'ob>,
    coding_system: Object,
    nocopy: Option<Object>,
    buffer: Option<Object>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    validate_string_input(string,env,cx)?;
    let coding = select(coding_system,env,cx)?;
    if buffer.is_some_and(|value| !value.is_nil()) {
        return Err(CodingError::DestinationUnsupported.into());
    }
    if matches!(coding, Coding::Identity) { return nil_coding_copy(string,nocopy,env,cx); }
    let bytes = input_bytes(string)?;
    if bytes.len() > MAX_BYTES {
        return Err(CodingError::SizeLimit.into());
    }
    if matches!(coding, Coding::Binary) {
        remember(env, "binary", cx)?;
        return Ok(
            if nocopy.is_some_and(|v| !v.is_nil())
                && matches!(string.untag(), ObjectType::ByteString(_))
            {
                string
            } else {
                cx.add(bytes.to_vec())
            },
        );
    }
    let (text, used) = decoded(bytes, coding)?;
    let unchanged = matches!(string.untag(),ObjectType::String(value) if &**value==text);
    remember(env, used, cx)?;
    // NOCOPY permits reuse, it does not require it. ByteString always becomes
    // valid Unicode storage here, including ASCII protocol sexps.
    Ok(if unchanged && nocopy.is_some_and(|v| !v.is_nil()) {
        string
    } else {
        cx.add(text)
    })
}
#[defun]
fn encode_coding_string<'ob>(
    string: Object<'ob>,
    coding_system: Object,
    nocopy: Option<Object>,
    buffer: Option<Object>,
    env: &mut Rt<Env>,
    cx: &'ob Context,
) -> Result<Object<'ob>> {
    validate_string_input(string,env,cx)?;
    let coding = select(coding_system,env,cx)?;
    if buffer.is_some_and(|value| !value.is_nil()) {
        return Err(CodingError::DestinationUnsupported.into());
    }
    if matches!(coding, Coding::Identity) { return nil_coding_copy(string,nocopy,env,cx); }
    let bytes = input_bytes(string)?;
    if bytes.len() > MAX_BYTES {
        return Err(CodingError::SizeLimit.into());
    }
    if matches!(coding, Coding::Binary) {
        remember(env, "binary", cx)?;
        return Ok(
            if nocopy.is_some_and(|v| !v.is_nil())
                && matches!(string.untag(), ObjectType::ByteString(_))
            {
                string
            } else {
                cx.add(bytes.to_vec())
            },
        );
    }
    if matches!(coding, Coding::Ascii) && !bytes.is_ascii() {
        return Err(CodingError::InvalidAscii.into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| CodingError::InvalidUtf8 { valid_up_to: error.valid_up_to() })?;
    let (output, used) = match coding {
        Coding::Dos => {
            if bytes
                .len()
                .checked_add(bytes.iter().filter(|b| **b == b'\n').count())
                .is_none_or(|n| n > MAX_BYTES)
            {
                return Err(CodingError::SizeLimit.into());
            }
            (text.replace('\n', "\r\n").into_bytes(), "utf-8-dos")
        }
        Coding::Mac => (text.replace('\n', "\r").into_bytes(), "utf-8-mac"),
        Coding::Ascii => (bytes.to_vec(), "us-ascii"),
        Coding::Utf8 => (bytes.to_vec(), "utf-8"),
        _ => (bytes.to_vec(), "utf-8-unix"),
    };
    remember(env, used, cx)?;
    Ok(cx.add(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{env::sym, gc::RootSet, object::ObjectType};
    use crate::interpreter::assert_lisp;
    #[test]
    fn raw_protocol_bytes_are_not_lossily_decoded() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let input = cx.add(vec![0xfe, 0xff]);
        assert!(matches!(input.untag(), ObjectType::ByteString(_)));
        let error = decoded(input_bytes(input).unwrap(), Coding::Utf8).unwrap_err();
        assert!(error.downcast_ref::<CodingError>().is_some());
        assert_eq!(input_bytes(input).unwrap(), &[0xfe, 0xff]);
        let input = cx.add("(:subject \"λ\")".as_bytes().to_vec());
        assert_eq!(
            decoded(input_bytes(input).unwrap(), Coding::Utf8).unwrap().0,
            "(:subject \"λ\")"
        );
    }
    #[test]
    fn actual_utf8_protocol_sexp_decode_and_read() {
        sym::init_symbols();
        assert_lisp(
            r#"(let ((x (encode-coding-string "(:subject \"λ\")" 'utf-8)))
            (list (multibyte-string-p x) (car (read-from-string (decode-coding-string x 'utf-8 t)))))"#,
            r#"(nil (:subject "λ"))"#,
        );
    }
    #[test]
    fn eol_autodetection_matches_gnu() {
        crate::assert_elprop!(
            "{}",
            r#"(progn (setq print-escape-newlines nil) (list
            (let ((x (decode-coding-string "a\r\nb\r\n" 'utf-8))) (list x last-coding-system-used))
            (let ((x (decode-coding-string "a\rb\r" 'utf-8))) (list x last-coding-system-used))
            (let ((x (decode-coding-string "a\r\nb\n" 'utf-8))) (list x last-coding-system-used))
            (let ((x (decode-coding-string "abc" 'utf-8))) (list x last-coding-system-used))))"#
        );
    }
    #[test]
    fn unsupported_coding_and_buffer_destination_are_explicit() {
        assert_lisp(
            r#"(list (condition-case nil (decode-coding-string "x" 'not-a-coding-system) (error 'caught))
            (condition-case nil (decode-coding-string "x" 3) (error 'caught))
            (condition-case nil (decode-coding-string "x" 'utf-8 nil t) (error 'caught)))"#,
            "(caught caught caught)",
        );
    }
}

#[cfg(test)]
mod locale_nil_tests {
    #[test]
    fn nil_coding_preserves_kind_properties_and_copy_identity_like_gnu() {
        for function in ["encode-coding-string","decode-coding-string"] {
            for nocopy in ["nil","t"] {
                crate::assert_elprop!("(let* ((value (list 42)) (s (propertize \"é\" 'face value)) (v ({} s nil {}))) (list (eq s v) (equal s v) (multibyte-string-p v) (eq (get-text-property 0 'face v) value) last-coding-system-used))",function,nocopy);
                crate::assert_elprop!(r#"(let* ((s (substring "\377" 0)) (v ({} s nil {}))) (list (eq s v) (equal s v) (multibyte-string-p v) (aref v 0) last-coding-system-used))"#,function,nocopy);
            }
        }
        crate::assert_elprop!(r#"(let ((v(vector nil))) (aset v 0 v) (condition-case e (encode-coding-string v nil) (wrong-type-argument (list (car e) (car(cdr e)) (eq v(car(cdr(cdr e))))))))"#);
    }
    #[test]
    fn empty_nil_coding_reuses_each_storage_kind_like_gnu() {
        for function in ["encode-coding-string","decode-coding-string"] {
            for nocopy in ["nil","t"] {
                for source in [r#""""#, "(make-string 0 0)", "(make-string 0 0 t)"] {
                    crate::assert_elprop!("(let* ((s {}) (v ({} s nil {}))) (list (eq s v) (eq (multibyte-string-p s) (multibyte-string-p v)) last-coding-system-used))",source,function,nocopy);
                }
            }
        }
    }

    #[test]
    fn native_locale_seed_is_nil_and_later_variable_bindings_are_real() {
        use crate::core::{env::{Env,sym},gc::{Context,RootSet}};
        use rune_core::macros::root;
        let roots=RootSet::default();let cx=&mut Context::new(&roots);
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        crate::core::env::init_variables(cx,env);
        assert!(crate::data::symbol_value(sym::LOCALE_CODING_SYSTEM,env,cx).unwrap().is_nil());
        let utf8=crate::core::env::intern("utf-8-unix",cx).into();
        env.varbind(sym::LOCALE_CODING_SYSTEM,utf8,cx).unwrap();
        assert!(crate::data::symbol_value(sym::LOCALE_CODING_SYSTEM,env,cx).unwrap().ptr_eq(utf8));
        cx.garbage_collect(true);
        assert_eq!(crate::data::symbol_value(sym::LOCALE_CODING_SYSTEM,env,cx).unwrap().to_string(),"utf-8-unix");
    }
}

#[cfg(test)]
mod structured_codec_boundary_tests {
    #[test]
    fn string_then_coding_type_precedence_matches_gnu() {
        for op in ["encode-coding-string","decode-coding-string"] {
            for (string,coding) in [("42","42"),("42","'bogus"),("42","nil"),("\"a\"","42")] {
                crate::assert_elprop!("(condition-case e ({} {} {}) (wrong-type-argument (list (car e) (car (cdr e)) (car (cdr (cdr e))))))",op,string,coding);
            }
        }
    }
    #[test]
    fn cyclic_invalid_codec_operands_signal_without_rendering() {
        for op in ["encode-coding-string","decode-coding-string"] {
            for object in ["(vector nil)","(list nil)","(make-byte-code 0 (make-string 0 0) (vector nil) 0)"] {
                // Cycles live in the vector/cons/bytecode constants. Each error
                // returns only predicate and pointer identity, never the graph.
                let mutate=if object.starts_with("(list") { "(setcar v v)" } else if object.starts_with("(vector") { "(aset v 0 v)" } else { "(aset (aref v 2) 0 v)" };
                crate::assert_elprop!("(let ((v {})) {} (list (condition-case e ({} v 42) (wrong-type-argument (list (car (cdr e)) (eq v (car (cdr (cdr e))))))) (condition-case e ({} \"a\" v) (wrong-type-argument (list (car (cdr e)) (eq v (car (cdr (cdr e)))))))))",object,mutate,op,op);
            }
        }
    }
}
