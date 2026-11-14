//! The explicitly supported, modifier-free Lisp `format` conversions.
//! No input is interpreted as a host printf/format program.
use crate::core::object::{Number, NumberType, NumberValue, Object, ObjectType};
use anyhow::{Result, bail};
use num_traits::ToPrimitive;
use std::fmt::Write;

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

fn float(number: Number, conversion: char) -> String {
    let value = number.to_float();
    if !value.is_finite() {
        return nonfinite(value);
    }
    let policy = match conversion {
        'f' => "%.6f",
        'e' => "%.6e",
        _ => "%.6g",
    };
    let mut text = crate::core::object::float_to_string_with_format(value, Some(policy));
    // The shared printer retains float syntax; printf-style %g does not.
    if conversion == 'g' && text.ends_with(".0") {
        text.truncate(text.len() - 2);
    }
    text
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
    let mut result = String::new();
    let mut arguments = objects.iter().copied();
    let mut characters = string.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            result.push(character);
            continue;
        }
        let Some(conversion) = characters.next() else {
            bail!("Format string ends in middle of format specifier")
        };
        if conversion == '%' {
            result.push('%');
            continue;
        }
        if !matches!(conversion, 'd' | 'o' | 'x' | 'X' | 'f' | 'e' | 'g' | 'c' | 's' | 'S') {
            bail!(
                "Unsupported format conversion or modifier %{conversion}; flags, width, precision and positional arguments are not implemented"
            );
        }
        let Some(argument) = arguments.next() else {
            bail!("Not enough arguments for format string")
        };
        match conversion {
            'd' => result.push_str(&integer(Number::try_from(argument)?, 10, false)?),
            'o' => result.push_str(&integer(Number::try_from(argument)?, 8, false)?),
            'x' | 'X' => {
                result.push_str(&integer(Number::try_from(argument)?, 16, conversion == 'X')?)
            }
            'f' | 'e' | 'g' => result.push_str(&float(Number::try_from(argument)?, conversion)),
            'c' => result.push(format_character(argument)?),
            's' => match argument.untag() {
                ObjectType::String(value) => result.push_str(value),
                ObjectType::ByteString(value) => result.push_str(std::str::from_utf8(value)?),
                _ => write!(result, "{argument}")?,
            },
            'S' => write!(result, "{argument}")?,
            _ => unreachable!("validated conversion"),
        }
    }
    // GNU evaluates but ignores extra arguments, including when there are no
    // conversions. They must not be rendered or rejected here.
    Ok(result)
}

#[cfg(test)]
mod tests {
    use crate::interpreter::assert_lisp;

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
            r#"(format "%.2f" 1.25)"#,
            r#"(format "%5d" 3)"#,
            r#"(format "%u" 3)"#,
            r#"(format "%😃" 3)"#,
        ] {
            assert_lisp(&format!("(condition-case nil {source} (error 'caught))"), "caught");
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
