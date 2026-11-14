//! Explicit GNU-style numeric/string format conversions, without host printf.
use crate::core::object::{Number, NumberType, NumberValue, Object, ObjectType};
use anyhow::{Context as _, Result, bail};
use num_traits::ToPrimitive;
use std::{fmt::Write, iter::Peekable, str::Chars};
use unicode_width::UnicodeWidthChar;

#[derive(Default)]
struct Spec {
    conversion: char,
    argument: Option<usize>,
    left: bool,
    plus: bool,
    space: bool,
    alternate: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
}

fn dimension(input: &mut Peekable<Chars<'_>>) -> Result<Option<usize>> {
    let mut value = None;
    while let Some(&character) = input.peek() {
        if !character.is_ascii_digit() {
            break;
        }
        input.next();
        let digit = usize::from(character as u8 - b'0');
        let next = value
            .unwrap_or(0_usize)
            .checked_mul(10)
            .and_then(|n| n.checked_add(digit))
            .context("format dimension overflow")?;
        if next > i32::MAX as usize {
            bail!("format dimension exceeds signed C-int range");
        }
        value = Some(next);
    }
    Ok(value)
}

fn spec(input: &mut Peekable<Chars<'_>>) -> Result<Spec> {
    let mut result = Spec::default();
    let mut positional = input.clone();
    if let Some(index) = dimension(&mut positional)? {
        if positional.peek() == Some(&'$') {
            if index == 0 {
                bail!("zero positional argument index is not implemented");
            }
            positional.next();
            *input = positional;
            result.argument = Some(index - 1);
        }
    }
    while let Some(&flag) = input.peek() {
        match flag {
            '-' => result.left = true,
            '+' => result.plus = true,
            ' ' => result.space = true,
            '#' => result.alternate = true,
            '0' => result.zero = true,
            _ => break,
        }
        input.next();
    }
    result.width = dimension(input)?.unwrap_or(0);
    if input.peek() == Some(&'$') || input.peek() == Some(&'*') {
        bail!("positional and dynamic format fields are not implemented");
    }
    if input.peek() == Some(&'.') {
        input.next();
        result.precision = Some(dimension(input)?.unwrap_or(0));
    }
    result.conversion = input.next().context("Format string ends in middle of format specifier")?;
    if !matches!(
        result.conversion,
        'd' | 'o' | 'x' | 'X' | 'f' | 'e' | 'g' | 'c' | 's' | 'S' | '%'
    ) {
        bail!("Unsupported format conversion %{}", result.conversion);
    }
    Ok(result)
}

fn reserve(text: &mut String, additional: usize) -> Result<()> {
    text.len().checked_add(additional).context("format output size overflow")?;
    text.try_reserve(additional).context("format output allocation failed")
}

fn repeat(text: &mut String, character: char, count: usize) -> Result<()> {
    reserve(text, count)?;
    text.extend(std::iter::repeat_n(character, count));
    Ok(())
}

fn columns(character: char) -> usize {
    match character {
        '\t' => 8,
        '\n' => 0,
        '\0'..='\u{1f}' | '\u{7f}' => 2,
        '\u{80}'..='\u{9f}' => 4,
        '\u{ad}' => 1,
        _ => character.width().unwrap_or(1),
    }
}

fn text_field(output: &mut String, mut text: String, field: &Spec) -> Result<()> {
    if field.conversion != 'c' {
        if let Some(limit) = field.precision {
            if limit == 0 {
                text.clear();
            } else {
                let mut width = 0_usize;
                let mut end = text.len();
                for (index, character) in text.char_indices() {
                    let next =
                        width.checked_add(columns(character)).context("format column overflow")?;
                    if next > limit {
                        end = index;
                        break;
                    }
                    width = next;
                }
                text.truncate(end);
            }
        }
    }
    let width = text
        .chars()
        .try_fold(0_usize, |n, ch| n.checked_add(columns(ch)).context("format column overflow"))?;
    let padding = field.width.saturating_sub(width);
    reserve(output, text.len().checked_add(padding).context("format output size overflow")?)?;
    if !field.left {
        repeat(output, ' ', padding)?;
    }
    output.push_str(&text);
    if field.left {
        repeat(output, ' ', padding)?;
    }
    Ok(())
}

fn nonfinite(value: f64) -> String {
    let sign = if value.is_sign_negative() { "-" } else { "" };
    format!("{sign}{}", if value.is_nan() { "nan" } else { "inf" })
}

fn integer(number: Number, radix: u32, uppercase: bool) -> Result<String> {
    let mut text = match number.untag() {
        NumberType::Int(value) if radix == 10 => value.to_string(),
        NumberType::Int(value) => {
            let magnitude = value.unsigned_abs();
            let digits =
                if radix == 8 { format!("{magnitude:o}") } else { format!("{magnitude:x}") };
            if value < 0 { format!("-{digits}") } else { digits }
        }
        NumberType::Big(value) => value.to_str_radix(radix),
        NumberType::Float(value) if !value.is_finite() && radix == 10 => nonfinite(**value),
        NumberType::Float(value) => {
            NumberValue::Float(**value).try_truncate()?.into_integer()?.to_str_radix(radix)
        }
    };
    if uppercase {
        text.make_ascii_uppercase();
    }
    Ok(text)
}

fn floating(number: Number, field: &Spec) -> Result<String> {
    let value = number.to_float();
    if !value.is_finite() {
        return Ok(nonfinite(value));
    }
    let precision = field.precision.unwrap_or(6);
    let mut text = String::new();
    reserve(&mut text, precision.checked_add(384).context("format precision overflow")?)?;
    match field.conversion {
        'f' => {
            write!(text, "{value:.precision$}")?;
            if field.alternate && precision == 0 {
                text.push('.');
            }
        }
        'e' => {
            let mut scientific = String::new();
            reserve(
                &mut scientific,
                precision.checked_add(32).context("format precision overflow")?,
            )?;
            write!(scientific, "{value:.precision$e}")?;
            let (coefficient, exponent) =
                scientific.split_once('e').context("internal scientific format")?;
            text.push_str(coefficient);
            if field.alternate && precision == 0 {
                text.push('.');
            }
            write!(text, "e{:+03}", exponent.parse::<i32>()?)?;
        }
        'g' => {
            let precision = precision.max(1);
            let mut scientific = String::new();
            reserve(
                &mut scientific,
                precision.checked_add(32).context("format precision overflow")?,
            )?;
            write!(scientific, "{value:.decimals$e}", decimals = precision - 1)?;
            let (coefficient, exponent) =
                scientific.split_once('e').context("internal general format")?;
            let exponent: i32 = exponent.parse()?;
            if exponent < -4 || i64::from(exponent) >= precision as i64 {
                if field.alternate {
                    text.push_str(coefficient);
                    if !coefficient.contains('.') {
                        text.push('.');
                    }
                } else {
                    text.push_str(coefficient.trim_end_matches('0').trim_end_matches('.'));
                }
                write!(text, "e{exponent:+03}")?;
            } else {
                let sign = if coefficient.starts_with('-') { "-" } else { "" };
                text.push_str(sign);
                let digits: String = coefficient.chars().filter(char::is_ascii_digit).collect();
                if exponent < 0 {
                    text.push_str("0.");
                    repeat(&mut text, '0', (-exponent - 1) as usize)?;
                    text.push_str(&digits);
                } else {
                    let whole = exponent as usize + 1;
                    if whole < digits.len() {
                        text.push_str(&digits[..whole]);
                        text.push('.');
                        text.push_str(&digits[whole..]);
                    } else {
                        text.push_str(&digits);
                        repeat(&mut text, '0', whole - digits.len())?;
                    }
                }
                if !field.alternate && text.contains('.') {
                    while text.ends_with('0') {
                        text.pop();
                    }
                    if text.ends_with('.') {
                        text.pop();
                    }
                } else if field.alternate && !text.contains('.') {
                    text.push('.');
                }
            }
        }
        _ => unreachable!("floating conversion"),
    }
    Ok(text)
}

fn numeric_field(
    output: &mut String,
    text: String,
    field: &Spec,
    is_integer: bool,
    finite: bool,
    float_decimal: bool,
) -> Result<()> {
    let negative = text.starts_with('-');
    let mut digits = if negative { &text[1..] } else { text.as_str() };
    let sign = if negative {
        "-"
    } else if field.plus {
        "+"
    } else if field.space {
        " "
    } else {
        ""
    };
    let zero_value = digits == "0";
    if is_integer && finite && field.precision == Some(0) && zero_value && !float_decimal {
        digits = "";
    }
    let zeros = if is_integer && finite {
        field.precision.unwrap_or(0).saturating_sub(digits.len())
    } else if is_integer && float_decimal {
        // GNU's float-to-decimal-integer fallback counts nonfinite text minus
        // one byte (including the requested sign) for minimum precision padding.
        field
            .precision
            .unwrap_or(0)
            .saturating_sub((digits.len() + sign.len()).saturating_sub(1))
    } else {
        0
    };
    let prefix = if field.alternate && is_integer && finite {
        match field.conversion {
            'o' if zeros == 0 && !digits.starts_with('0') => "0",
            'x' if !zero_value => "0x",
            'X' if !zero_value => "0X",
            _ => "",
        }
    } else {
        ""
    };
    let content = sign
        .len()
        .checked_add(prefix.len())
        .and_then(|n| n.checked_add(zeros))
        .and_then(|n| n.checked_add(digits.len()))
        .context("format field size overflow")?;
    let padding = field.width.saturating_sub(content);
    reserve(output, content.checked_add(padding).context("format output size overflow")?)?;
    let zero_padding =
        field.zero && !field.left && finite && (!is_integer || field.precision.is_none());
    if !field.left && !zero_padding {
        repeat(output, ' ', padding)?;
    }
    output.push_str(sign);
    output.push_str(prefix);
    if zero_padding {
        repeat(output, '0', padding)?;
    }
    repeat(output, '0', zeros)?;
    output.push_str(digits);
    if field.left {
        repeat(output, ' ', padding)?;
    }
    Ok(())
}

// Limits apply only to this supported list/vector traversal, not all legacy
// object display or the outer format string's independent field allocations.
const NESTED_NODE_LIMIT: usize = 100_000;
const NESTED_BYTE_LIMIT: usize = 16 * 1024 * 1024;

enum PrintTask<'a> {
    Object(Object<'a>),
    Tail(Object<'a>),
    Cons(Object<'a>),
    Text(&'static str),
    Leave(usize),
    QuoteDepth(usize),
}

fn append_nested(output: &mut String, text: &str) -> Result<()> {
    if output.len().checked_add(text.len()).context("format nested size overflow")? > NESTED_BYTE_LIMIT {
        bail!("format nested object byte limit exceeded");
    }
    reserve(output, text.len())?;
    output.push_str(text);
    Ok(())
}

fn nested_object(object: Object, readable: bool) -> Result<String> {
    use crate::core::env::sym;
    use std::collections::HashSet;
    let mut tasks = Vec::new();
    tasks.try_reserve(1)?;
    tasks.push(PrintTask::Object(object));
    let mut active = HashSet::new();
    let mut output = String::new();
    let mut steps = 0_usize;
    let mut quote_depth = 0_usize;
    while let Some(task) = tasks.pop() {
        steps += 1;
        if steps > NESTED_NODE_LIMIT {
            bail!("format nested object traversal limit exceeded");
        }
        tasks.try_reserve(4)?;
        match task {
            PrintTask::Text(text) => {
                append_nested(&mut output, text)?;
            }
            PrintTask::Leave(id) => { active.remove(&id); }
            PrintTask::QuoteDepth(depth) => { quote_depth = depth; }
            PrintTask::Tail(tail) => match tail.untag() {
                ObjectType::NIL => {}
                ObjectType::Cons(_) => {
                    tasks.push(PrintTask::Cons(tail));
                    tasks.push(PrintTask::Text(" "));
                }
                _ => {
                    tasks.push(PrintTask::Object(tail));
                    tasks.push(PrintTask::Text(" . "));
                }
            },
            PrintTask::Cons(value) => {
                let id = value.into_raw().addr();
                if active.contains(&id) {
                    bail!("circular objects in format are not implemented");
                }
                active.try_reserve(1)?;
                active.insert(id);
                let ObjectType::Cons(cons) = value.untag() else { unreachable!() };
                tasks.push(PrintTask::Leave(id));
                tasks.push(PrintTask::Tail(cons.cdr()));
                tasks.push(PrintTask::Object(cons.car()));
            }
            PrintTask::Object(value) => match value.untag() {
                ObjectType::Cons(cons) => {
                    // GNU abbreviates these exact builtin symbols only when
                    // followed by exactly one argument in a proper list.
                    let prefix = match cons.car().untag() {
                        ObjectType::Symbol(sym::QUOTE) => Some("'"),
                        ObjectType::Symbol(sym::FUNCTION) => Some("#'"),
                        ObjectType::Symbol(sym::BACKQUOTE) => Some("`"),
                        ObjectType::Symbol(sym::UNQUOTE) if quote_depth > 0 => Some(","),
                        ObjectType::Symbol(sym::SPLICE) if quote_depth > 0 => Some(",@"),
                        _ => None,
                    };
                    if let (Some(prefix), ObjectType::Cons(tail)) = (prefix, cons.cdr().untag()) {
                        if matches!(tail.cdr().untag(), ObjectType::NIL) {
                            let id = value.into_raw().addr();
                            if active.contains(&id) {
                                bail!("circular objects in format are not implemented");
                            }
                            active.try_reserve(1)?;
                            active.insert(id);
                            tasks.push(PrintTask::Leave(id));
                            tasks.push(PrintTask::QuoteDepth(quote_depth));
                            quote_depth = match prefix {
                                "`" => quote_depth.checked_add(1).context("format quote depth overflow")?,
                                "," | ",@" => quote_depth - 1,
                                _ => quote_depth,
                            };
                            tasks.push(PrintTask::Object(tail.car()));
                            tasks.push(PrintTask::Text(prefix));
                            continue;
                        }
                    }
                    tasks.push(PrintTask::Text(")"));
                    tasks.push(PrintTask::Cons(value));
                    tasks.push(PrintTask::Text("("));
                }
                ObjectType::Vec(vector) => {
                    let id = value.into_raw().addr();
                    if active.contains(&id) {
                        bail!("circular objects in format are not implemented");
                    }
                    if vector.len() > NESTED_NODE_LIMIT.saturating_sub(tasks.len()) / 2 {
                        bail!("format nested object traversal limit exceeded");
                    }
                    active.try_reserve(1)?;
                    active.insert(id);
                    tasks.try_reserve(vector.len() * 2 + 2)?;
                    tasks.push(PrintTask::Leave(id));
                    tasks.push(PrintTask::Text("]"));
                    for (index, item) in vector.iter().enumerate().rev() {
                        tasks.push(PrintTask::Object(item.get()));
                        if index != 0 { tasks.push(PrintTask::Text(" ")); }
                    }
                    tasks.push(PrintTask::Text("["));
                }
                ObjectType::String(value) => {
                    let escaped = if readable { value.chars().filter(|ch| matches!(ch, '\\' | '"')).count() + 2 } else { 0 };
                    if value.len().checked_add(escaped).context("format nested size overflow")?
                        > NESTED_BYTE_LIMIT.saturating_sub(output.len()) {
                        bail!("format nested object byte limit exceeded");
                    }
                    let text = if readable { readable_string(&value)? } else { (**value).to_owned() };
                    append_nested(&mut output, &text)?;
                }
                ObjectType::Symbol(value) => {
                    let text = if readable { readable_symbol(value.name())? } else { value.name().to_owned() };
                    append_nested(&mut output, &text)?;
                }
                ObjectType::Record(_) | ObjectType::HashTable(_) | ObjectType::ByteFn(_) | ObjectType::ByteString(_) => {
                    bail!("this nested object type in format is not implemented");
                }
                _ => {
                    let text = value.to_string();
                    append_nested(&mut output, &text)?;
                }
            },
        }
        if output.len() > NESTED_BYTE_LIMIT || tasks.len() > NESTED_NODE_LIMIT {
            bail!("format nested object resource limit exceeded");
        }
    }
    Ok(output)
}

fn readable_symbol(name: &str) -> Result<String> {
    if name.is_empty() {
        return Ok("##".to_owned());
    }
    let quote_first = name.starts_with(['.', '?']) || crate::reader::numeric_symbol_name(name);
    let mut text = String::new();
    reserve(&mut text, name.len())?;
    for (index, character) in name.char_indices() {
        if (index == 0 && quote_first)
            || character <= ' '
            || matches!(character, '\\' | '"' | '\'' | '`' | ',' | '(' | ')' | '[' | ']' | ';' | '#')
        {
            reserve(&mut text, 1)?;
            text.push('\\');
        }
        reserve(&mut text, character.len_utf8())?;
        text.push(character);
    }
    Ok(text)
}

fn readable_string(value: &str) -> Result<String> {
    let mut text = String::new();
    let escapes = value.chars().filter(|ch| matches!(ch, '\\' | '"')).count();
    let capacity = value.len().checked_add(escapes).and_then(|n| n.checked_add(2))
        .context("format quoted string size overflow")?;
    reserve(&mut text, capacity)?;
    text.push('"');
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            text.push('\\');
        }
        text.push(character);
    }
    text.push('"');
    Ok(text)
}

fn format_character(object: Object) -> Result<char> {
    let code = match object.untag() {
        ObjectType::Int(value) => u32::try_from(value).ok(),
        ObjectType::BigInt(value) => value.to_u32(),
        _ => None,
    };
    code.and_then(char::from_u32)
        .ok_or_else(|| anyhow::anyhow!("format %c requires a Unicode scalar character"))
}

pub(super) fn format(string: &str, objects: &[Object]) -> Result<String> {
    let mut output = String::new();
    let mut next_argument = 0_usize;
    let mut input = string.chars().peekable();
    while let Some(character) = input.next() {
        if character != '%' {
            reserve(&mut output, character.len_utf8())?;
            output.push(character);
            continue;
        }
        let field = spec(&mut input)?;
        if field.conversion == '%' {
            output.push('%');
            continue;
        }
        let index = field.argument.unwrap_or(next_argument);
        let argument = objects.get(index).copied().context("Not enough arguments for format string")?;
        next_argument = index.checked_add(1).context("format argument index overflow")?;
        match field.conversion {
            'd' | 'o' | 'x' | 'X' | 'f' | 'e' | 'g' => {
                let number = Number::try_from(argument)?;
                let is_integer = matches!(field.conversion, 'd' | 'o' | 'x' | 'X');
                let finite = match number.untag() {
                    NumberType::Float(value) => value.is_finite(),
                    _ if is_integer => true,
                    _ => number.to_float().is_finite(),
                };
                let text = if is_integer {
                    integer(
                        number,
                        match field.conversion {
                            'o' => 8,
                            'x' | 'X' => 16,
                            _ => 10,
                        },
                        field.conversion == 'X',
                    )?
                } else {
                    floating(number, &field)?
                };
                numeric_field(
                    &mut output,
                    text,
                    &field,
                    is_integer,
                    finite,
                    field.conversion == 'd' && number.is_float(),
                )?;
            }
            'c' => text_field(&mut output, format_character(argument)?.to_string(), &field)?,
            's' | 'S' => {
                let text = if matches!(argument.untag(), ObjectType::Cons(_) | ObjectType::Vec(_)) {
                    nested_object(argument, field.conversion == 'S')?
                } else if field.conversion == 's' {
                    match argument.untag() {
                        ObjectType::String(value) => (**value).to_owned(),
                        ObjectType::ByteString(value) => std::str::from_utf8(value)?.to_owned(),
                        _ => argument.to_string(),
                    }
                } else {
                    match argument.untag() {
                        ObjectType::String(value) => readable_string(&value)?,
                        ObjectType::Symbol(value) => readable_symbol(value.name())?,
                        _ => argument.to_string(),
                    }
                };
                text_field(&mut output, text, &field)?;
            }
            _ => unreachable!("validated conversion"),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use crate::interpreter::assert_lisp;

    #[test]
    fn nested_aggregate_and_quote_matrix_matches_gnu() {
        let expressions = [
            "'(a . b)", "'(quote a b)", "'(quote)", "'(quote nil)",
            "'(function (lambda (x) x))", "'(function a b)",
            r"'(\` ((\, x) (\,@ y)))", r"'(\, x)", r"'(\,@ x)",
            r"'(\` (\` ((\, x) (\, (\, y)))))",
            r"'(\` (quote ((\, x))))",
            r#"(cons "a" "b")"#,
            r#"(list "a\"b" "c\\d" (make-symbol "1.0"))"#,
            r#"(vector [] nil (list "λ" (make-symbol "a b")))"#,
            r#"(let ((x (list "shared"))) (cons x x))"#,
            r#"(let ((x (vector "shared"))) (vector x x))"#,
        ];
        for descriptor in ["", "30", "-30", "20.12", ".0", ".1"] {
            let mut form = String::from("(progn (setq print-circle nil print-escape-newlines nil) (list");
            for conversion in ['s', 'S'] {
                for expression in expressions {
                    form.push_str(&format!(" (format \"%{descriptor}{conversion}\" {expression})"));
                }
            }
            form.push_str("))");
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn nested_lists_vectors_and_sharing_match_gnu() {
        crate::assert_elprop!("{}", r#"(progn (setq print-circle nil print-escape-newlines nil)
            (let ((shared (list "x" (make-symbol "a b"))))
                (list (format "%s" (list shared shared))
                    (format "%S" (list shared shared))
                    (format "%S" (cons shared 3))
                    (format "%s" (vector shared "a\"b"))
                    (format "%S" (vector shared "a\"b"))
                    (format "%S" '(quote (function x)))
                    (format "%S" '(\, x))
                    (format "%S" '(\` ((\, x) (\,@ y))))
                    (format "%12.7S" (list "x" "y")))))"#);
    }

    #[test]
    fn circular_aggregates_have_explicit_documented_divergence() {
        crate::library::elprop::start_emacs().unwrap();
        for (source, gnu) in [
            (r#"(let ((x (list 1))) (setcdr x x) (format "%S" x))"#, r#""(1 . #0)""#),
            (r#"(let ((x (vector nil))) (aset x 0 x) (format "%S" x))"#, r#""[#0]""#),
            (r#"(let ((x (list nil))) (setcar x x) (format "%S" x))"#, r#""(#0)""#),
            (r#"(let* ((x (list nil)) (v (vector x))) (setcar x v) (format "%S" x))"#, r#""([#0])""#),
            (r#"(let* ((x (list 1)) (y (list 2 x))) (setcar x y) (format "%S" x))"#, r#""((2 #0))""#),
        ] {
            let reference = format!("(progn (setq print-circle nil) {source})");
            assert_eq!(crate::library::elprop::eval(&reference).unwrap(), gnu);
            assert_lisp(&format!("(condition-case nil {source} (error 'caught))"), "caught");
        }
    }

    #[test]
    fn nested_output_limit_rejects_before_growing_destination() {
        let text = "x".repeat(super::NESTED_BYTE_LIMIT);
        let mut output = String::from("prefix");
        let capacity = output.capacity();
        assert!(super::append_nested(&mut output, &text).is_err());
        assert_eq!(output, "prefix");
        assert_eq!(output.capacity(), capacity);
    }

    #[test]
    fn nested_traversal_is_iterative_and_has_a_work_limit() {
        use crate::core::{cons::Cons, gc::{Context, RootSet}, object::Object};
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let mut value = Object::from(1_i32);
        for _ in 0..5_000 {
            value = Cons::new1(value, &cx).into();
        }
        let output = super::nested_object(value, true).unwrap();
        assert_eq!(output.len(), 10_001);
        assert!(output.starts_with("((("));
        for _ in 0..25_000 {
            value = Cons::new1(value, &cx).into();
        }
        let error = super::nested_object(value, true).unwrap_err();
        assert!(error.to_string().contains("limit"));
    }

    #[test]
    fn readable_symbol_names_match_gnu() {
        crate::assert_elprop!("{}", r#"(progn (setq print-circle nil print-escape-newlines nil)
            (mapcar (function (lambda (name) (format "%S" (make-symbol name))))
                '("" "a b" "a\\b" "a\"b" "a(b" "a)b" "a[b" "a]b" "a;b"
                  "a#b" "a?b" "a.b" "." ".." ".x" "1" "1." "1.0" "+1" "-1"
                  "1e2" "1e+NaN" "1foo" "?a" ":x" "a\nb" "a,b" "a`b" "a'b"
                  "+" "-" "inf" "nan" "1e+" "x|y" "λ")))"#);
    }

    #[test]
    fn readable_top_level_string_escaping_matches_gnu() {
        crate::assert_elprop!("{}", r#"(progn
            (setq print-circle nil print-escape-newlines nil print-escape-control-characters nil)
            (list (format "%S" "a\"b") (format "%S" "a\\b")
                (format "%12.7S" "a\"b\\c")
                (format "%S" (string 0 9 10 13 27 955))))"#);
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_utf8_without_arguments_is_bounded(
            input in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::prelude::Just('%'),
                    proptest::char::any(),
                    proptest::sample::select("0123456789$.*+- #".chars().collect::<Vec<_>>()),
                ], 0..128)
        ) {
            let input: String = input.into_iter().collect();
            if let Ok(output) = super::format(&input, &[]) {
                proptest::prop_assert!(output.len() <= input.len());
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn integer_layout_preserves_value(
            value in proptest::prelude::any::<i32>(),
            width in 0_usize..40,
            precision in 1_usize..25,
            conversion in proptest::sample::select(vec!['d', 'o', 'x', 'X']),
            flags in proptest::sample::select(vec!["", "+", " ", "0", "-", "+0", "-+0"])
        ) {
            let descriptor = format!("%{flags}{width}.{precision}{conversion}");
            let output = super::format(&descriptor, &[crate::core::object::Object::from(value)]).unwrap();
            let radix = match conversion { 'o' => 8, 'x' | 'X' => 16, _ => 10 };
            let parsed = i64::from_str_radix(output.trim(), radix).unwrap();
            proptest::prop_assert_eq!(parsed, i64::from(value));
            proptest::prop_assert!(output.len() >= width);
        }
    }

    #[test]
    fn extreme_numeric_layout_matches_gnu() {
        let values = ["5.0e-324", "-5.0e-324", "1.7976931348623157e308",
            "-1.7976931348623157e308", "9223372036854775808.0",
            "-9223372036854775808.0", "9007199254740991.0", "9007199254740992.0",
            "999999.5", "0.00009999995", "-0.0", "-1.0e+NaN",
            "1.0e+INF", "-1.0e+INF"];
        for descriptor in ["", "+020", " #24.0", "#.1", "-24.6", "030.17"] {
            let mut form = String::from("(progn (setq print-circle nil print-escape-newlines nil) (list");
            for conversion in ['d', 'o', 'x', 'X', 'f', 'e', 'g'] {
                for value in values {
                    form.push_str(&format!(" (condition-case nil (format \"%{descriptor}{conversion}\" {value}) (error 'caught))"));
                }
            }
            form.push_str("))");
            assert!(form.len() < 16 * 1024);
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn positional_layout_matrix_matches_gnu() {
        for descriptor in ["", "8", "-8", "+08", "#.0", " #12.4", "02.3"] {
            let mut form = String::from("(progn (setq print-circle nil print-escape-newlines nil) (list");
            for conversion in ['d', 'o', 'x', 'X', 'f', 'e', 'g', 's', 'S'] {
                for index in [1, 2, 3] {
                    form.push_str(&format!(" (format \"%{index}${descriptor}{conversion}:%1$s:%s\" -7 1.25 0)"));
                }
            }
            form.push_str("))");
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn positional_fields_match_gnu() {
        crate::assert_elprop!("{}", r#"(list (format "%2$s:%1$d" 7 "x")
            (format "%3$s:%1$s:%s" "a" "b" "c")
            (format "%2$5s" "a" "b") (format "%02$s" "a" "b")
            (format "%1$%:%s" "a") (format "%2$+08.2f" 0 1.25))"#);
    }

    #[test]
    fn simple_numeric_and_object_conversions_match_gnu() {
        for (source, expected) in [
            (r#"(format "%x" 255)"#, r#""ff""#),
            (r#"(format "%X" 255)"#, r#""FF""#),
            (r#"(format "%x" -1)"#, r#""-1""#),
            (r#"(format "%o" -9)"#, r#""-11""#),
            (r#"(format "%x" 18446744073709551616)"#, r#""10000000000000000""#),
            (r#"(format "%d" 1.9)"#, r#""1""#),
            (r#"(format "%d" -0.0)"#, r#""0""#),
            (r#"(format "%d" -1.0e+INF)"#, r#""-inf""#),
            (r#"(format "%f" 1.5)"#, r#""1.500000""#),
            (r#"(format "%e" 1234.0)"#, r#""1.234000e+03""#),
            (r#"(format "%g" 1.0)"#, r#""1""#),
            (r#"(format "%g" -0.0)"#, r#""-0""#),
            (r#"(format "%f" 1.0e+NaN)"#, r#""nan""#),
            (r#"(format "%c" 955)"#, r#""λ""#),
            (r#"(format "%S" "x")"#, r#""\"x\"""#),
            (r#"(format "%s" nil)"#, r#""nil""#),
            (r#"(format "%s" 1 2)"#, r#""1""#),
            (r#"(format "\\%d" 3)"#, r#""\\3""#),
            (r#"(format "%%")"#, r#""%""#),
            (r#"(format "λ:%x" 255)"#, r#""λ:ff""#),
            (r#"(format "%s" (unibyte-string 65 66))"#, r#""AB""#),
            (r#"(equal (format "%s" "a\"b\\c") "a\"b\\c")"#, "t"),
        ] {
            assert_lisp(source, expected);
        }
        for source in [
            r#"(format "%c" 65.9)"#,
            r#"(format "%x" 1.0e+INF)"#,
            r#"(format "%d" 'bad)"#,
            r#"(format "%")"#,
            r#"(format "%s")"#,
            r#"(format "%3$s" 1 2)"#,
            r#"(format "%99999999999999999999999999999d" 1)"#,
            r#"(format "%u" 3)"#,
            r#"(format "%😃" 3)"#,
        ] {
            assert_lisp(&format!("(condition-case nil {source} (error 'caught))"), "caught");
        }
    }

    #[test]
    fn width_precision_and_flags_match_gnu_numeric_matrix() {
        let values = [
            "0",
            "-12",
            "15",
            "1.25",
            "-1.25",
            "-0.0",
            "(ash 1 2000)",
            "1.0e+INF",
            "-1.0e+INF",
            "-1.0e+NaN",
        ];
        for descriptor in [
            "", "5", "05", "-5", "+5", " 5", "#", "#08", ".0", ".2", ".4", "8.4", "08.4", "-8.4",
            "+#12.4", " #012.4", "#.0",
        ] {
            let mut form =
                String::from("(progn (setq print-circle nil print-escape-newlines nil) (list");
            for conversion in ['d', 'o', 'x', 'X', 'f', 'e', 'g'] {
                for value in values {
                    form.push_str(&format!(" (condition-case nil (format \"%{descriptor}{conversion}\" {value}) (error 'caught))"));
                }
            }
            form.push_str("))");
            assert!(form.len() < 16 * 1024);
            eprintln!("checking GNU descriptor {descriptor}");
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn floating_precision_beyond_literal_printer_limits_matches_gnu() {
        for precision in [0, 1, 2, 6, 15, 17, 20, 40] {
            let mut form =
                String::from("(progn (setq print-circle nil print-escape-newlines nil) (list");
            for conversion in ['f', 'e', 'g'] {
                for value in [
                    "-0.0",
                    "1.2345678901234567",
                    "9.999999999999999",
                    "1e-5",
                    "5e-324",
                    "1.7976931348623157e308",
                ] {
                    form.push_str(&format!(" (format \"%.{precision}{conversion}\" {value})"));
                }
            }
            form.push_str("))");
            crate::assert_elprop!("{}", form);
        }
        for (source, expected) in [
            (r#"(format "%.0d" -0.0)"#, r#""0""#),
            (r#"(format "%.0d" 0)"#, r#""""#),
            (r#"(format "%.4d" 1.0e+INF)"#, r#""00inf""#),
            (r#"(format "%+.4d" 1.0e+INF)"#, r#""+0inf""#),
            (r#"(format "%#08x" -15)"#, r#""-0x0000f""#),
            (r#"(format "%8.2f" 1.25)"#, r#""    1.25""#),
        ] {
            assert_lisp(source, expected);
        }
    }

    #[test]
    fn width_precision_and_unicode_default_columns_match_gnu() {
        let values = [
            r#""abc""#,
            r#""aé🙂z""#,
            r#""界a""#,
            r#""éa""#,
            r#""👨‍👩""#,
            r#""\t""#,
            r#""\n""#,
            "(string 0)",
            r#""­a""#,
        ];
        for descriptor in ["5", "-5", "05", ".0", ".1", ".3", ".4", "8.3", "-8.3"] {
            let mut form =
                String::from("(progn (setq print-circle nil print-escape-newlines nil) (list");
            for conversion in ['s', 'S'] {
                for value in values {
                    form.push_str(&format!(" (format \"%{descriptor}{conversion}\" {value})"));
                }
            }
            form.push_str("))");
            eprintln!("checking GNU text descriptor {descriptor}");
            crate::assert_elprop!("{}", form);
        }
    }

    #[test]
    fn aggregate_simple_numeric_formats_match_gnu() {
        let values = [
            "0",
            "-1",
            "255",
            "18446744073709551616",
            "-18446744073709551616",
            "1.9",
            "-1.9",
            "-0.0",
            "1e100",
            "1.7976931348623157e308",
            "5e-324",
            "1.0e+INF",
            "-1.0e+INF",
            "1.0e+NaN",
            "-1.0e+NaN",
        ];
        let mut form = String::from("(list");
        for conversion in ['d', 'o', 'x', 'X', 'f', 'e', 'g'] {
            for value in values {
                form.push_str(&format!(
                    " (condition-case nil (format \"%{conversion}\" {value}) (error 'caught))"
                ));
            }
        }
        form.push(')');
        assert!(form.len() < 16 * 1024);
        crate::assert_elprop!("{}", form);
    }
}
