//! Small explicit coding layer for the real mu byte-framed protocol.
//! Invalid UTF8 is rejected, never replaced or mapped byte-for-Unicode.
use crate::core::{
    env::{Env, intern},
    error::{Type, TypeError},
    gc::{Context, Rt},
    object::{Object, ObjectType},
};
use anyhow::Result;
use rune_macros::defun;
defvar!(LAST_CODING_SYSTEM_USED);

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
    Utf8,
    Unix,
    Dos,
    Mac,
    Binary,
    Ascii,
}
fn select(value: Object) -> Result<Coding> {
    let ObjectType::Symbol(symbol) = value.untag() else {
        return Err(TypeError::new(Type::Symbol, value.untag()).into());
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
        other => Err(TypeError::new(Type::String, other).into()),
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
    env.set_var(intern("last-coding-system-used", cx), intern(name, cx).into())
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
    let coding = select(coding_system)?;
    if buffer.is_some_and(|value| !value.is_nil()) {
        return Err(CodingError::DestinationUnsupported.into());
    }
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
    let coding = select(coding_system)?;
    if buffer.is_some_and(|value| !value.is_nil()) {
        return Err(CodingError::DestinationUnsupported.into());
    }
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
