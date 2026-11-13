//! Lisp reader that reads an object from a string.
use crate::core::{
    env::{intern, sym},
    gc::Context,
    object::{Object, Symbol},
};
use crate::fns;
use num_bigint::BigInt;
use rune_core::macros::list;
use std::str;
use std::{fmt, iter::Peekable, str::CharIndices};
use std::{fmt::Display, str::FromStr};

type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during reading a sexp from a string
#[derive(PartialEq, Debug, Copy, Clone)]
pub(crate) enum Error {
    MissingCloseParen(usize),
    MissingCloseBracket(usize),
    MissingStringDel(usize),
    MissingQuotedItem(usize),
    ExtraItemInCdr(usize),
    ExtraCloseParen(usize),
    ExtraCloseBracket(usize),
    UnexpectedChar(char, usize),
    UnknownMacroCharacter(char, usize),
    ParseInt(u8, usize),
    MalformedUnicdoe(usize),
    InvalidEscape(usize),
    EmptyStream,
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::MissingCloseParen(i) => write!(f, "Missing close paren: at {i}"),
            Error::MissingCloseBracket(i) => write!(f, "Missing close bracket: at {i}"),
            Error::MissingStringDel(i) => write!(f, "Missing closing string quote: at {i}"),
            Error::ExtraCloseParen(i) => write!(f, "Extra Closing paren: at {i}"),
            Error::ExtraCloseBracket(i) => write!(f, "Extra Closing brace: at {i}"),
            Error::UnexpectedChar(chr, i) => write!(f, "Unexpected character {chr}: at {i}"),
            Error::MalformedUnicdoe(i) => write!(f, "Malformed unicode: at {i}"),
            Error::InvalidEscape(i) => write!(f, "Invalid or unsupported character escape: at {i}"),
            Error::EmptyStream => write!(f, "Empty Stream"),
            Error::ExtraItemInCdr(i) => write!(f, "Extra item in cdr: at {i}"),
            Error::MissingQuotedItem(i) => write!(f, "Missing element after quote: at {i}"),
            Error::ParseInt(radix, i) => {
                write!(f, "invalid character for radix {radix}: at {i}")
            }
            Error::UnknownMacroCharacter(chr, i) => {
                write!(f, "Unkown reader macro character {chr}: at {i}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    fn mut_pos(&mut self) -> Option<&mut usize> {
        match self {
            Error::MissingCloseParen(i)
            | Error::MissingCloseBracket(i)
            | Error::MissingStringDel(i)
            | Error::UnexpectedChar(_, i)
            | Error::MalformedUnicdoe(i)
            | Error::InvalidEscape(i)
            | Error::ExtraItemInCdr(i)
            | Error::ExtraCloseParen(i)
            | Error::ExtraCloseBracket(i)
            | Error::MissingQuotedItem(i)
            | Error::UnknownMacroCharacter(_, i)
            | Error::ParseInt(_, i) => Some(i),
            Error::EmptyStream => None,
        }
    }

    pub(crate) fn update_pos(&mut self, offset: usize) {
        if let Some(pos) = self.mut_pos() {
            *pos += offset;
        }
    }
}

#[derive(PartialEq, Debug, Copy, Clone)]
enum Token<'a> {
    OpenParen(usize),
    CloseParen(usize),
    OpenBracket(usize),
    CloseBracket(usize),
    Quote(usize),
    Backquote(usize),
    Unquote(usize),
    Splice(usize),
    Sharp(usize),
    Dot(usize),
    QuestionMark(usize, char),
    Ident(&'a str),
    String(&'a str),
}

impl Display for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Token::OpenParen(_) => write!(f, "("),
            Token::CloseParen(_) => write!(f, ")"),
            Token::OpenBracket(_) => write!(f, "["),
            Token::CloseBracket(_) => write!(f, "]"),
            Token::Quote(_) => write!(f, "'"),
            Token::Backquote(_) => write!(f, "`"),
            Token::Unquote(_) => write!(f, ","),
            Token::Splice(_) => write!(f, ",@"),
            Token::Sharp(_) => write!(f, "#"),
            Token::Dot(_) => write!(f, "."),
            Token::QuestionMark(_, chr) => write!(f, "?{chr}"),
            Token::Ident(x) => write!(f, "{x}"),
            Token::String(x) => write!(f, "\"{x}\""),
        }
    }
}

#[derive(Clone)]
struct Tokenizer<'a> {
    slice: &'a str,
    iter: Peekable<CharIndices<'a>>,
}

impl<'a> Tokenizer<'a> {
    fn new(slice: &'a str) -> Self {
        Self { slice, iter: slice.char_indices().peekable() }
    }

    /// Given a [`Token`] calculate it's position relative to this `Tokenizer`.
    fn relative_pos(&self, token: Token<'a>) -> usize {
        match token {
            Token::OpenParen(x)
            | Token::CloseParen(x)
            | Token::OpenBracket(x)
            | Token::CloseBracket(x)
            | Token::Quote(x)
            | Token::Backquote(x)
            | Token::Unquote(x)
            | Token::Splice(x)
            | Token::Sharp(x)
            | Token::Dot(x)
            | Token::QuestionMark(x, _) => x,
            Token::Ident(slice) | Token::String(slice) => {
                let beg = self.slice.as_ptr() as usize;
                let end = slice.as_ptr() as usize;
                end - beg
            }
        }
    }

    /// Return the current position of the Tokenizer. This is the index of the
    /// next character.
    fn cur_pos(&mut self) -> usize {
        match self.iter.peek() {
            Some((idx, _)) => *idx,
            None => self.slice.len(),
        }
    }

    /// Skip characters until the closure returns true.
    fn skip_till(&mut self, mut func: impl FnMut(char) -> bool) -> usize {
        while self.iter.next_if(|x| !func(x.1)).is_some() {}
        match self.iter.peek() {
            Some((idx, _)) => *idx,
            None => self.slice.len(),
        }
    }

    /// Skip whitespace and comments until the next valid read character.
    fn skip_till_char(&mut self) {
        let mut in_comment = false;
        let valid_char = |chr: char| {
            if in_comment {
                if chr == '\n' {
                    in_comment = false;
                }
                false
            } else if chr.is_ascii_whitespace() {
                false
            } else if chr == ';' {
                in_comment = true;
                false
            } else {
                true
            }
        };
        self.skip_till(valid_char);
    }

    fn get_string(&mut self, open_delim_pos: usize) -> Result<Token<'a>> {
        let mut skip = false;
        let idx_chr = self.iter.find(|(_, chr)| !escaped(&mut skip, *chr) && *chr == '"');
        match idx_chr {
            Some((end, '"')) => Ok(Token::String(&self.slice[(open_delim_pos + 1)..end])),
            _ => Err(Error::MissingStringDel(open_delim_pos)),
        }
    }

    fn get_symbol(&mut self, beg: usize, chr: char) -> Result<Token<'a>> {
        let mut skip = chr == '\\';
        let end = self.skip_till(|c| !escaped(&mut skip, c) && !symbol_char(c));
        // An unmatched quoting backslash requests another source character.
        // GNU lread.c:read_symbol signals end-of-file rather than dropping it.
        if skip {
            return Err(Error::MissingQuotedItem(end));
        }
        Ok(Token::Ident(&self.slice[beg..end]))
    }

    /// After having found a `,`, see if the next token is a `@` or not.
    fn get_macro_char(&mut self, idx: usize) -> Token<'a> {
        match self.iter.next_if(|(_, chr)| *chr == '@') {
            Some(_) => Token::Splice(idx),
            None => Token::Unquote(idx),
        }
    }

    fn read_quoted_char(&mut self, idx: usize) -> Result<Token<'a>> {
        match self.iter.next() {
            Some((start, item)) => {
                if item == '\\' {
                    // Character escapes have their own boundaries: a control
                    // target can itself be whitespace or a list delimiter.
                    let Some((_, chr)) = self.iter.next() else {
                        return Err(Error::MissingQuotedItem(start));
                    };
                    let value = read_character_escape(chr, &mut self.iter, start)?;
                    if matches!(self.iter.peek(), Some((_, chr)) if symbol_char(*chr) && *chr != '?')
                    {
                        return Err(Error::InvalidEscape(start));
                    }
                    Ok(Token::QuestionMark(start, value))
                } else {
                    match self.iter.peek() {
                        Some((i, chr)) if symbol_char(*chr) && *chr != '?' => {
                            Err(Error::UnexpectedChar(*chr, *i)) // ?aa
                        }
                        _ => Ok(Token::QuestionMark(idx, item)), // ?a
                    }
                }
            }
            None => Err(Error::MissingQuotedItem(idx)),
        }
    }

    fn read_char(&mut self) -> Option<char> {
        self.iter.next().map(|x| x.1)
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Result<Token<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.skip_till_char();
        let (idx, chr) = self.iter.next()?;
        let token = match chr {
            '(' => Ok(Token::OpenParen(idx)),
            ')' => Ok(Token::CloseParen(idx)),
            '[' => Ok(Token::OpenBracket(idx)),
            ']' => Ok(Token::CloseBracket(idx)),
            '\'' => Ok(Token::Quote(idx)),
            ',' => Ok(self.get_macro_char(idx)),
            '`' => Ok(Token::Backquote(idx)),
            '#' => Ok(Token::Sharp(idx)),
            // GNU lread.c's dot dispatch deliberately excludes adjacent ')' and
            // ']': `.)` reads the symbol '.', but `. )` is dotted-list syntax.
            '.' if self.iter.peek().is_none_or(|(_, chr)| {
                matches!(
                    *chr,
                    '\x00'..=' ' | '\u{a0}' | '"' | '\'' | ';' | '(' | '[' | '#' | '?' | '`' | ','
                )
            }) =>
            {
                Ok(Token::Dot(idx))
            }
            '?' => self.read_quoted_char(idx),
            '"' => self.get_string(idx),
            other if symbol_char(other) => self.get_symbol(idx, other),
            unknown => Err(Error::UnexpectedChar(unknown, idx)),
        };
        Some(token)
    }
}

/// Remove reader quoting without applying number parsing or symbol shorthands.
fn symbol_name(symbol: &str) -> String {
    let mut chars = symbol.chars();
    let mut name = String::with_capacity(symbol.len());
    while let Some(chr) = chars.next() {
        name.push(if chr == '\\' { chars.next().unwrap_or(chr) } else { chr });
    }
    name
}

fn intern_symbol<'ob>(symbol: &str, cx: &'ob Context) -> Symbol<'ob> {
    if symbol.contains('\\') {
        intern(&symbol_name(symbol), cx)
    } else {
        intern(symbol, cx)
    }
}

/// Parse a symbol from a string. This will either by a true symbol or a number
/// literal.
fn parse_symbol<'a>(slice: &str, cx: &'a Context) -> Object<'a> {
    // A trailing decimal point denotes an integer in Lisp, not a Rust float.
    let integer = slice.strip_suffix('.').unwrap_or(slice);
    let digits = integer.strip_prefix(['+', '-']).unwrap_or(integer);
    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        if let Ok(num) = integer.parse::<i64>() {
            // The tagged integer payload is narrower than i64. Normalize at
            // the reader boundary instead of losing high bits while boxing.
            if (crate::arith::MIN_FIXNUM..=crate::arith::MAX_FIXNUM).contains(&num) {
                return cx.add(num);
            }
        }
        if let Ok(num) = BigInt::from_str(integer) {
            return cx.add(num);
        }
    }
    match lisp_float(slice) {
        Some(num) => cx.add(num),
        None => cx.add(intern_symbol(slice, cx)),
    }
}

/// Recognise Lisp's decimal spelling before asking Rust to convert its value.
/// In particular, Rust's bare `inf`, `nan` and `infinity` are Lisp symbols.
fn lisp_float(slice: &str) -> Option<f64> {
    let unsigned = slice.strip_prefix(['+', '-']).unwrap_or(slice);
    let mut chars = unsigned.chars().peekable();
    let mut digits = 0;
    while chars.next_if(char::is_ascii_digit).is_some() {
        digits += 1;
    }
    let dot = chars.next_if_eq(&'.').is_some();
    let mut fraction = 0;
    if dot {
        while chars.next_if(char::is_ascii_digit).is_some() {
            fraction += 1;
        }
    }
    if digits + fraction == 0 {
        return None;
    }
    if chars.next_if(|chr| matches!(chr, 'e' | 'E')).is_some() {
        let exponent: String = chars.collect();
        match exponent.as_str() {
            "+INF" => {
                return Some(if slice.starts_with('-') {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                });
            }
            // The current float object preserves NaN classification/sign, not GNU's payload.
            "+NaN" => return Some(if slice.starts_with('-') { -f64::NAN } else { f64::NAN }),
            _ => {}
        }
        let exponent_digits = exponent.strip_prefix(['+', '-']).unwrap_or(&exponent);
        if exponent_digits.is_empty() || !exponent_digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
    } else if chars.next().is_some() || !dot || fraction == 0 {
        return None;
    }
    slice.parse::<f64>().ok()
}

/// Decode the scalar-valued subset that the current Rust UTF-8 string model can
/// represent. Modified keycodes (Meta/Super/etc.) and raw-byte string metadata
/// need the later character/string representation; never silently return NUL.
fn read_character_escape(
    first: char,
    chars: &mut Peekable<impl Iterator<Item = (usize, char)>>,
    pos: usize,
) -> Result<char> {
    let value = match first {
        'a' => '\u{07}',
        'b' => '\u{08}',
        'd' => '\u{7f}',
        'e' => '\u{1b}',
        'f' => '\u{0c}',
        'n' => '\n',
        'r' => '\r',
        's' if !matches!(chars.peek(), Some((_, '-'))) => ' ',
        't' => '\t',
        'v' => '\u{0b}',
        'C' | '^' => {
            if first == 'C' && chars.next().map(|(_, chr)| chr) != Some('-') {
                return Err(Error::InvalidEscape(pos));
            }
            let (_, character) = chars.next().ok_or(Error::InvalidEscape(pos))?;
            match character {
                '?' => '\u{7f}',
                ' ' | '@'..='_' | 'a'..='z' => {
                    char::from_u32((character as u32) & 0x1f).ok_or(Error::InvalidEscape(pos))?
                }
                _ => return Err(Error::InvalidEscape(pos)),
            }
        }
        'x' | 'u' | 'U' | '0'..='7' => {
            let radix = if first.is_ascii_digit() { 8 } else { 16 };
            let (minimum, maximum) = match first {
                'u' => (4, 4),
                'U' => (8, 8),
                'x' => (1, usize::MAX),
                _ => (1, 3),
            };
            let mut count = usize::from(first.is_ascii_digit());
            let mut code = first.to_digit(8).unwrap_or(0);
            while count < maximum {
                let Some(digit) = chars.peek().and_then(|(_, chr)| chr.to_digit(radix)) else {
                    break;
                };
                chars.next();
                code = code
                    .checked_mul(radix)
                    .and_then(|n| n.checked_add(digit))
                    .ok_or(Error::MalformedUnicdoe(pos))?;
                count += 1;
            }
            if count < minimum {
                return Err(Error::MalformedUnicdoe(pos));
            }
            char::from_u32(code).ok_or(Error::MalformedUnicdoe(pos))?
        }
        'M' | 'S' | 'H' | 'A' | 's' if matches!(chars.peek(), Some((_, '-'))) => {
            return Err(Error::InvalidEscape(pos));
        }
        other => other,
    };
    Ok(value)
}

/// process escape characters in the string slice and return the resulting
/// string.
fn unescape_string<'a>(string: &str, offset: usize, cx: &'a Context) -> Result<Object<'a>> {
    let mut new = cx.string_with_capacity(string.len());
    let mut chars = string.char_indices().peekable();
    while let Some((index, chr)) = chars.next() {
        let pos = offset + index;
        if chr != '\\' {
            new.push(chr);
            continue;
        }
        let (_, escaped) = chars.next().ok_or(Error::InvalidEscape(pos))?;
        if escaped != '\n' && escaped != ' ' {
            new.push(read_character_escape(escaped, &mut chars, pos)?);
        }
    }
    Ok(cx.add(new))
}

/// Return true if `chr` is a valid symbol character.
const fn symbol_char(chr: char) -> bool {
    !matches!(chr, '\x00'..=' ' | '(' | ')' | '[' | ']' | '#' | ',' | '`' | ';' | '"' | '\'')
}

fn escaped(escaped: &mut bool, chr: char) -> bool {
    if *escaped {
        *escaped = false;
        true
    } else if chr == '\\' {
        *escaped = true;
        true
    } else {
        false
    }
}

/// State of the reader.
struct Reader<'a, 'ob> {
    /// The iterator over the tokens in the current slice.
    tokens: Tokenizer<'a>,
    /// New objects are allocated in the context.
    cx: &'ob Context<'ob>,
}

impl<'a, 'ob> Reader<'a, 'ob> {
    /// Read the cdr of a literal list.
    /// ```lisp
    /// '(1 2 3 . 45)
    ///           ^^^
    /// ```
    fn read_cdr(&mut self, delim: usize) -> Result<Object<'ob>> {
        match self.tokens.next() {
            Some(Ok(sexp)) => {
                let obj = self.read_sexp(sexp)?;
                match self.tokens.next() {
                    Some(Ok(Token::CloseParen(_))) => Ok(obj),
                    Some(Ok(token)) => Err(Error::ExtraItemInCdr(self.tokens.relative_pos(token))),
                    Some(Err(e)) => Err(e),
                    None => Err(Error::MissingCloseParen(delim)),
                }
            }
            Some(Err(e)) => Err(e),
            None => Err(Error::MissingCloseParen(delim)),
        }
    }

    fn read_list(&mut self, delim: usize) -> Result<Object<'ob>> {
        let mut objects = Vec::new();
        while let Some(token) = self.tokens.next() {
            match token? {
                Token::CloseParen(_) => return Ok(fns::slice_into_list(&objects, None, self.cx)),
                Token::Dot(pos) => {
                    if objects.is_empty() {
                        return Err(Error::UnexpectedChar('.', pos));
                    }
                    let cdr = self.read_cdr(delim)?;
                    return Ok(fns::slice_into_list(&objects, Some(cdr), self.cx));
                }
                tok => objects.push(self.read_sexp(tok)?),
            }
        }
        Err(Error::MissingCloseParen(delim))
    }

    fn read_vec(&mut self, delim: usize) -> Result<Object<'ob>> {
        let mut objects = self.cx.vec_new();
        while let Some(token) = self.tokens.next() {
            match token? {
                Token::CloseBracket(_) => return Ok(self.cx.add(objects)),
                tok => objects.push(self.read_sexp(tok)?),
            }
        }
        Err(Error::MissingCloseBracket(delim))
    }

    /// Quote an item using `symbol`.
    fn quote_item(&mut self, pos: usize, symbol: Symbol) -> Result<Object<'ob>> {
        match self.tokens.next() {
            Some(token) => {
                let obj = self.read_sexp(token?)?;
                Ok(list!(symbol, obj; self.cx))
            }
            None => Err(Error::MissingQuotedItem(pos)),
        }
    }

    /// Read number with specificed radix
    fn read_radix(&mut self, pos: usize, radix: u8) -> Result<Object<'ob>> {
        if !(2..=36).contains(&radix) {
            return Err(Error::ParseInt(radix, pos));
        }

        // Radix syntax does not skip whitespace between the prefix and digits
        // (GNU lread.c:read_integer). Preserve arbitrary precision and signs.
        let start = self.tokens.cur_pos();
        let end = self.tokens.skip_till(|chr| !symbol_char(chr));
        let ident = &self.tokens.slice[start..end];
        let digits = ident.strip_prefix(['+', '-']).unwrap_or(ident);
        if digits.is_empty() || !digits.chars().all(|chr| chr.is_digit(radix.into())) {
            return Err(Error::ParseInt(radix, pos));
        }
        if let Ok(value) = i64::from_str_radix(ident, radix.into()) {
            if (crate::arith::MIN_FIXNUM..=crate::arith::MAX_FIXNUM).contains(&value) {
                return Ok(self.cx.add(value));
            }
        }
        match BigInt::parse_bytes(ident.as_bytes(), radix.into()) {
            Some(value) => Ok(self.cx.add(value)),
            None => Err(Error::ParseInt(radix, pos)),
        }
    }

    /// Read a symbol after #: or #_. These dispatch forms never read numbers;
    /// an absent name denotes the empty symbol without consuming its delimiter.
    fn read_sharp_symbol(&mut self, uninterned: bool) -> Result<Object<'ob>> {
        let name = match self.tokens.iter.peek().copied() {
            Some((start, chr)) if symbol_char(chr) => {
                self.tokens.iter.next();
                let Token::Ident(raw) = self.tokens.get_symbol(start, chr)? else {
                    unreachable!();
                };
                symbol_name(raw)
            }
            _ => String::new(),
        };
        let symbol = if uninterned {
            Symbol::new_uninterned(&name, self.cx)
        } else {
            intern(&name, self.cx)
        };
        Ok(self.cx.add(symbol))
    }

    /// read a sharp quoted character. This could be used for reader macro's in
    /// the future, but right now it just handles the special cases from elisp.
    fn read_sharp(&mut self, pos: usize) -> Result<Object<'ob>> {
        match self.tokens.read_char() {
            Some('\'') => match self.tokens.next() {
                Some(Ok(Token::OpenParen(i))) => {
                    let list = self.read_list(i)?;
                    Ok(list!(sym::FUNCTION, list; self.cx))
                }
                Some(token) => {
                    let obj = self.read_sexp(token?)?;
                    Ok(list!(sym::FUNCTION, obj; self.cx))
                }
                None => Err(Error::MissingQuotedItem(pos)),
            },
            Some('b' | 'B') => self.read_radix(pos, 2),
            Some('o' | 'O') => self.read_radix(pos, 8),
            Some('x' | 'X') => self.read_radix(pos, 16),
            Some(':') => self.read_sharp_symbol(true),
            Some('_') => self.read_sharp_symbol(false),
            Some('#') => Ok(self.cx.add(intern("", self.cx))),
            Some(chr) if chr.is_ascii_digit() => {
                let mut radix = (chr as u8) - b'0';
                loop {
                    match self.tokens.read_char() {
                        Some('r' | 'R') => break,
                        Some(chr) if chr.is_ascii_digit() => {
                            match radix
                                .checked_mul(10)
                                .and_then(|r| r.checked_add(chr as u8 - b'0'))
                            {
                                Some(r) => radix = r,
                                // TODO: Better error for radix overflow
                                None => return Err(Error::UnknownMacroCharacter(chr, pos)),
                            }
                        }
                        Some(chr) => return Err(Error::UnknownMacroCharacter(chr, pos)),
                        None => return Err(Error::MissingQuotedItem(pos)),
                    }
                }
                self.read_radix(pos, radix)
            }
            Some(chr) => Err(Error::UnknownMacroCharacter(chr, pos)),
            None => Err(Error::MissingQuotedItem(pos)),
        }
    }

    fn read_sexp(&mut self, token: Token<'a>) -> Result<Object<'ob>> {
        match token {
            Token::OpenParen(i) => self.read_list(i),
            Token::CloseParen(i) => Err(Error::ExtraCloseParen(i)),
            Token::OpenBracket(i) => self.read_vec(i),
            Token::CloseBracket(i) => Err(Error::ExtraCloseBracket(i)),
            Token::Quote(i) => self.quote_item(i, sym::QUOTE),
            Token::Unquote(i) => self.quote_item(i, sym::UNQUOTE),
            Token::Splice(i) => self.quote_item(i, sym::SPLICE),
            Token::Backquote(i) => self.quote_item(i, sym::BACKQUOTE),
            Token::Sharp(i) => self.read_sharp(i),
            Token::Dot(i) => Err(Error::UnexpectedChar('.', i)),
            Token::QuestionMark(_, c) => Ok((c as i64).into()),
            Token::Ident(x) => Ok(parse_symbol(x, self.cx)),
            Token::String(x) => unescape_string(x, self.tokens.relative_pos(token), self.cx),
        }
    }
}

/// read a lisp object from `slice`. Return the object and index of next
/// remaining character in the slice.
pub(crate) fn read<'ob>(slice: &str, cx: &'ob Context) -> Result<(Object<'ob>, usize)> {
    let mut reader = Reader { tokens: Tokenizer::new(slice), cx };
    match reader.tokens.next() {
        Some(Ok(t)) => reader.read_sexp(t).map(|x| (x, reader.tokens.cur_pos())),
        Some(Err(e)) => Err(e),
        None => Err(Error::EmptyStream),
    }
}

#[cfg(test)]
mod test {
    use crate::core::{cons::Cons, gc::RootSet};

    use super::*;

    #[test]
    fn tokens() {
        let mut iter = Tokenizer::new("1 foo (\"bar\" . 1.3)");
        assert_eq!(iter.next(), Some(Ok(Token::Ident("1"))));
        assert_eq!(iter.next(), Some(Ok(Token::Ident("foo"))));
        assert_eq!(iter.next(), Some(Ok(Token::OpenParen(6))));
        assert_eq!(iter.next(), Some(Ok(Token::String("bar"))));
        assert_eq!(iter.next(), Some(Ok(Token::Dot(13))));
        assert_eq!(iter.next(), Some(Ok(Token::Ident("1.3"))));
        assert_eq!(iter.next(), Some(Ok(Token::CloseParen(18))));
        assert_eq!(iter.next(), None);
    }

    macro_rules! check_reader {
        ($expect:expr, $compare:expr, $cx:expr) => {{
            let obj = $cx.add($expect);
            assert_eq!(obj, read($compare, $cx).unwrap().0)
        }};
    }

    #[test]
    fn test_read_number() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        check_reader!(5, "5", cx);
        check_reader!(49, "49", cx);
        check_reader!(-105, "-105", cx);
        check_reader!(1.5, "1.5", cx);
        check_reader!(-3.0, "-3.0", cx);
        check_reader!(1, "+1", cx);
        check_reader!(1, "001", cx);
        check_reader!(1, "#o001", cx);
        check_reader!(8, "#o10", cx);
        check_reader!(8, "#8r10", cx);
        check_reader!(2385, "#o4521", cx);
        check_reader!(2385, "#8r4521", cx);
        check_reader!(0b1, "#b001", cx);
        check_reader!(0b10, "#b10", cx);
        check_reader!(0b10, "#2r10", cx);
        check_reader!(0b10, "#00000002r10", cx);
        check_reader!(0b101_1101, "#b1011101", cx);
        check_reader!(0x1, "#x001", cx);
        check_reader!(0x10, "#x10", cx);
        check_reader!(0xdead_beef_i64, "#xDeAdBeEf", cx);
        check_reader!(171, "#12r0123", cx);
        check_reader!(49360, "#36r1234", cx);
        assert_error("#37r1234", Error::ParseInt(37, 0), cx);
        assert_error("#257r1234", Error::UnknownMacroCharacter('7', 0), cx);
        assert_error("#123456r1234", Error::UnknownMacroCharacter('4', 0), cx);
    }

    #[test]
    #[expect(clippy::non_ascii_literal)]
    fn test_read_char() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        check_reader!(97, "?a", cx);
        check_reader!(21, "?", cx);
        check_reader!(225, "?á", cx);
        check_reader!(97, "?a?a", cx);
        check_reader!(97, "?a#'foo ?a", cx);
        assert_error("?aa", Error::UnexpectedChar('a', 2), cx);
        assert_error("?", Error::MissingQuotedItem(0), cx);
    }

    #[test]
    fn read_bool() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        check_reader!(false, "nil", cx);
        check_reader!(false, "()", cx);
        check_reader!(true, "t", cx);
    }

    #[test]
    fn test_read_symbol() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        check_reader!(sym::IF, "if", cx);
        check_reader!(intern("--1", cx), "--1", cx);
        check_reader!(intern("1", cx), "\\1", cx);
        check_reader!(intern("3.0.0", cx), "3.0.0", cx);
        check_reader!(intern("1+", cx), "1+", cx);
        check_reader!(intern("+1", cx), "\\+1", cx);
        check_reader!(intern(" x", cx), "\\ x", cx);
        check_reader!(intern("\\x", cx), "\\\\x", cx);
        check_reader!(intern("x.y", cx), "x.y", cx);
        check_reader!(intern("(* 1 2)", cx), "\\(*\\ 1\\ 2\\)", cx);
        check_reader!(intern("+-*/_~!@$%^&=:<>{}", cx), "+-*/_~!@$%^&=:<>{}", cx);
    }

    #[test]
    fn test_read_string() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        check_reader!("foo", r#""foo""#, cx);
        check_reader!("foo bar", r#""foo bar""#, cx);
        check_reader!("foo\nbar\t\r", r#""foo\nbar\t\r""#, cx);
        check_reader!(
            "foobarbaz",
            r#""foo\ bar\
baz""#,
            cx
        );
    }

    #[test]
    fn test_read_cons() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        check_reader!(false, "()", cx);
        check_reader!(Cons::new(1, 2, cx), "(1 . 2)", cx);
        check_reader!(list!(1; cx), "(1)", cx);
        check_reader!(list!("foo"; cx), "(\"foo\")", cx);
        check_reader!(Cons::new(1, Cons::new(1.5, "foo", cx), cx), "(1 1.5 . \"foo\")", cx);
        check_reader!(list!("foo", Cons::new(1, 1.5, cx); cx), "(\"foo\" (1 . 1.5))", cx);
        check_reader!(list!(1, 1.5; cx), "(1 1.5)", cx);
        check_reader!(list!(1, 1.5, -7; cx), "(1 1.5 -7)", cx);
        check_reader!(list!(1, 1.5, intern(".", cx); cx), "(1 1.5 .)", cx);
        check_reader!(list!(1, 1.5, intern("...", cx), 2; cx), "(1 1.5 ... 2)", cx);
    }

    #[test]
    fn read_quote() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        let quote = sym::QUOTE;
        check_reader!(list!(quote, sym::IF; cx), "(quote if)", cx);
        check_reader!(list!(quote, sym::IF; cx), "'if", cx);
        check_reader!(list!(quote, list!(1, 2, 3; cx); cx), "'(1 2 3)", cx);
        check_reader!(u32::from('a'), "?a", cx);
        check_reader!(u32::from(' '), "?\\s", cx);
        check_reader!(u32::from('\t'), "?\\t", cx);
        check_reader!(u32::from('\u{AFD}'), "?\\u0afd", cx);
        check_reader!(0xabc_u32, "?\\xabc", cx);
    }

    #[test]
    fn read_sharp() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        let quote = sym::FUNCTION;
        check_reader!(list!(quote, sym::IF; cx), "#'if", cx);
        check_reader!(
            list!(quote, list!(intern("lambda", cx), sym::IF, false, false; cx); cx),
            "#'(lambda if () nil)",
            cx
        );
        assert_error("#", Error::MissingQuotedItem(0), cx);
        assert_error("#'", Error::MissingQuotedItem(0), cx);
        assert_error("#a", Error::UnknownMacroCharacter('a', 0), cx);
    }

    #[test]
    fn test_read_vec() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        check_reader!(Vec::<Object>::new(), "[]", cx);
        let vec: Vec<Object> = vec![1.into()];
        check_reader!(vec, "[1]", cx);
        let vec: Vec<Object> = vec![1.into(), 2.into()];
        check_reader!(vec, "[1 2]", cx);
        let vec: Vec<Object> = vec![1.into(), 2.into(), 3.into()];
        check_reader!(vec, "[1 2 3]", cx);
    }

    fn assert_error(input: &str, error: Error, cx: &Context) {
        let result = read(input, cx).err().unwrap();
        assert_eq!(result, error);
    }

    #[test]
    fn reader_error() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_error("", Error::EmptyStream, cx);
        assert_error(" (1 2", Error::MissingCloseParen(1), cx);
        assert_error("  (1 (2 3) 4", Error::MissingCloseParen(2), cx);
        assert_error("  (1 (2 3 4", Error::MissingCloseParen(5), cx);
        assert_error(" \"foo", Error::MissingStringDel(1), cx);
        assert_error("(1 2 . 3 4)", Error::ExtraItemInCdr(9), cx);
        assert_error("(1 3 \0)", Error::UnexpectedChar('\0', 5), cx);
        assert_error(" '", Error::MissingQuotedItem(1), cx);
        assert_error(" )", Error::ExtraCloseParen(1), cx);
        assert_error("(1 . #o9 3)", Error::ParseInt(8, 5), cx);
    }

    #[test]
    fn comments() {
        let roots = &RootSet::default();
        let cx = &Context::new(roots);
        assert_error(" ; comment ", Error::EmptyStream, cx);
        check_reader!(1, "; comment \n  1", cx);
    }
}

/// Narrow M0 regressions; these do not claim the full RD-01 reader or GNU's
/// raw-byte/multibyte string representation and modified character set.
#[cfg(test)]
mod m0_reader_regressions {
    use super::*;
    use crate::core::{cons::Cons, gc::RootSet, object::ObjectType};

    #[test]
    fn dotted_lists_require_one_cdr_after_a_nonempty_prefix() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, expected) in [
            ("(1 . 2)", Cons::new(1, 2, &cx)),
            ("(1 .?a)", Cons::new(1, 97, &cx)),
            ("(1 .(2))", Cons::new(1, list!(2; &cx), &cx)),
            ("(1 .; comment\n2)", Cons::new(1, 2, &cx)),
        ] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(expected), "{source}");
        }
        for (source, position) in
            [(".", 0), (". ", 0), ("(. 1)", 1), ("[. 1]", 1), ("(1 . . 2)", 5)]
        {
            assert_eq!(
                read(source, &cx).unwrap_err(),
                Error::UnexpectedChar('.', position),
                "{source}"
            );
        }
        assert_eq!(read("(1 . )", &cx).unwrap_err(), Error::ExtraCloseParen(5));
        assert_eq!(read("(1 . 2 3)", &cx).unwrap_err(), Error::ExtraItemInCdr(7));
        assert_eq!(read("(1 . 2", &cx).unwrap_err(), Error::MissingCloseParen(0));
    }

    #[test]
    fn adjacent_close_delimiter_and_quoted_dots_remain_symbols() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let dot = intern(".", &cx);
        for source in ["(1 .)", r"(1 \.)"] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(list!(1, dot; &cx)), "{source}");
        }
        assert_eq!(read("[.]", &cx).unwrap().0, cx.add(vec![cx.add(dot)]));
        assert_eq!(read(r"\.", &cx).unwrap().0, cx.add(dot));
        for source in [".5", "1.", "...", ".foo"] {
            assert!(read(source, &cx).is_ok(), "{source}");
        }
    }

    #[test]
    fn decimal_literals_normalize_at_fixnum_boundaries() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for value in [crate::arith::MIN_FIXNUM, crate::arith::MAX_FIXNUM] {
            for source in [value.to_string(), format!("{value}.")] {
                let object = read(&source, &cx).unwrap().0;
                assert!(matches!(object.untag(), ObjectType::Int(_)), "{source}");
                assert_eq!(object, cx.add(value), "{source}");
            }
        }
        for value in
            [crate::arith::MIN_FIXNUM - 1, crate::arith::MAX_FIXNUM + 1, i64::MIN, i64::MAX]
        {
            for source in [value.to_string(), format!("{value}.")] {
                let object = read(&source, &cx).unwrap().0;
                assert!(matches!(object.untag(), ObjectType::BigInt(_)), "{source}");
                assert_eq!(object, cx.add(BigInt::from(value)), "{source}");
            }
        }
    }

    #[test]
    fn dangling_symbol_quotes_are_read_errors() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for source in ["\\", "name\\", "(name\\", "#:name\\", "#_name\\"] {
            assert_eq!(
                read(source, &cx).unwrap_err(),
                Error::MissingQuotedItem(source.len()),
                "{source}"
            );
        }
        // Complete pairs quote an actual backslash and must remain readable.
        for source in [r"name\\", r"#_name\\"] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(intern("name\\", &cx)));
        }
        assert_eq!(read(r"a\ b", &cx).unwrap().0, cx.add(intern("a b", &cx)));
    }

    #[test]
    fn m0_reader_signed_and_large_radix_integers() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, value) in [
            ("#x-ff", -255_i64),
            ("#X+FF", 255),
            ("#B-10", -2),
            ("#O+77", 63),
            ("#16R-fF", -255),
            ("#36rZ", 35),
        ] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(value), "{source}");
        }
        for (source, digits, radix) in [
            ("#x7fffffffffffffff", "7fffffffffffffff", 16),
            ("#x-8000000000000000", "-8000000000000000", 16),
            ("#x10000000000000000", "10000000000000000", 16),
            ("#x-10000000000000000", "-10000000000000000", 16),
            (
                "#2r11111111111111111111111111111111111111111111111111111111111111111",
                "11111111111111111111111111111111111111111111111111111111111111111",
                2,
            ),
        ] {
            let ObjectType::BigInt(actual) = read(source, &cx).unwrap().0.untag() else {
                panic!("large radix integer must not wrap: {source}");
            };
            let expected = cx.add(BigInt::parse_bytes(digits.as_bytes(), radix).unwrap());
            assert_eq!(cx.add(actual), expected, "{source}");
        }
        for source in ["#x ff", "#x\n10", "#b;comment\n10", "#x+", "#x-", "#b2", "#xfg"] {
            assert!(matches!(read(source, &cx), Err(Error::ParseInt(_, 0))), "{source}");
        }
        assert_eq!(read("#Xff next", &cx).unwrap(), (cx.add(255_i64), 4));
    }

    #[test]
    fn m0_reader_sharp_symbol_dispatch() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, name) in
            [("#:alpha", "alpha"), ("#:123", "123"), (r"#:a\ b", "a b"), ("#:", "")]
        {
            let ObjectType::Symbol(first) = read(source, &cx).unwrap().0.untag() else {
                panic!("uninterned dispatch must read a symbol");
            };
            assert_eq!(first.name(), name);
            assert_ne!(cx.add(first), cx.add(intern(name, &cx)));
            assert_ne!(cx.add(first), read(source, &cx).unwrap().0);
        }
        for (source, name) in [("##", ""), ("#_123", "123"), ("#_", ""), (r"#_a\ b", "a b")] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(intern(name, &cx)), "{source}");
        }
        // An empty name leaves the closing delimiter to the surrounding reader.
        assert_eq!(read("[#: ## #_123]", &cx).unwrap().1, 13);
        assert_eq!(read("#: next", &cx).unwrap().1, 2);
    }

    #[test]
    fn m0_reader_rust_float_words_remain_symbols() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for name in ["inf", "nan", "infinity", "NaN", "INF", "+inf", "-infinity", "1e", "+", "-"] {
            assert_eq!(read(name, &cx).unwrap().0, cx.add(intern(name, &cx)), "{name}");
        }
    }

    #[test]
    fn m0_reader_trailing_decimal_point_is_integer() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, value) in [("1.", 1_i64), ("-25.", -25), ("+001.", 1), ("0.", 0)] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(value), "{source}");
        }
        let digits = "123456789012345678901234567890";
        assert_eq!(
            read(&format!("{digits}."), &cx).unwrap().0,
            cx.add(BigInt::from_str(digits).unwrap())
        );
        assert_eq!(read(r"\1.", &cx).unwrap().0, cx.add(intern("1.", &cx)));
    }

    #[test]
    fn m0_reader_decimal_float_grammar() {
        for (source, value) in [
            ("1.5", 1.5),
            (".5", 0.5),
            ("-.5", -0.5),
            ("1e3", 1000.0),
            ("1E-3", 0.001),
            ("1.e2", 100.0),
        ] {
            assert_eq!(lisp_float(source), Some(value), "{source}");
        }
        for source in ["inf", "nan", "1.", ".", "1e", "1e+", ".e2", "1.2.3", "1_0.5", "1e2x"] {
            assert_eq!(lisp_float(source), None, "{source}");
        }
        assert!(lisp_float("-0.0").unwrap().is_sign_negative());
    }

    #[test]
    fn m0_reader_special_float_syntax() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, negative) in [("1.0e+INF", false), ("-1.0e+INF", true)] {
            let ObjectType::Float(value) = read(source, &cx).unwrap().0.untag() else {
                panic!("special infinity must be a float");
            };
            assert!(value.is_infinite());
            assert_eq!(value.is_sign_negative(), negative);
        }
        for source in ["0.0e+NaN", "1.5e+NaN", "-0.0e+NaN"] {
            let ObjectType::Float(value) = read(source, &cx).unwrap().0.untag() else {
                panic!("special NaN must be a float");
            };
            assert!(value.is_nan());
            assert_eq!(value.is_sign_negative(), source.starts_with('-'));
        }
    }

    #[test]
    fn m0_reader_ascii_control_character_literals() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, value) in [
            (r"?\C-a", 1_i64),
            (r"?\C-z", 26),
            (r"?\^A", 1),
            (r"?\C-?", 127),
            (r"?\C-@", 0),
            (r"?\C-_", 31),
            ("?\\C- ", 0),
            (r"?\C-]", 29),
        ] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(value), "{source}");
        }
        let (_, next) = read(r"?\C-a next", &cx).unwrap();
        assert_eq!(next, 5);
        // Escaped literals, like plain character literals, leave the next
        // character literal and enclosing delimiter for the next read.
        assert_eq!(read(r"?\n?a", &cx).unwrap(), (cx.add(10_i64), 3));
        assert_eq!(read(r"[?\C-]]", &cx).unwrap().1, 7);
    }

    #[test]
    fn m0_reader_numeric_character_escapes() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, value) in [
            (r"?\101", 65_i64),
            (r"?\x41", 65),
            (r"?\u0041", 65),
            (r"?\U0001F600", 0x1f600),
            (r"?\000", 0),
        ] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(value), "{source}");
        }
    }

    #[test]
    fn m0_reader_common_string_escapes() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        let source = r#""\a\b\d\e\f\n\r\s\t\v\C-a\^B\101\x42\u0043\U0001F600""#;
        let expected = "\u{07}\u{08}\u{7f}\u{1b}\u{0c}\n\r \t\u{0b}\u{01}\u{02}ABC\u{1f600}";
        assert_eq!(read(source, &cx).unwrap().0, cx.add(expected));
        assert_eq!(read("\"a\\ \u{e9}\\\nb\"", &cx).unwrap().0, cx.add("a\u{e9}b"));
    }

    #[test]
    fn m0_reader_string_numeric_escape_boundaries() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for (source, expected) in [
            (r#""\u0041F""#, "AF"),
            (r#""\1012""#, "A2"),
            (r#""\x41F""#, "\u{41f}"),
            (r#""\\n\"""#, "\\n\""),
        ] {
            assert_eq!(read(source, &cx).unwrap().0, cx.add(expected), "{source}");
        }
    }

    #[test]
    fn m0_reader_malformed_numeric_escapes_are_errors() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for source in [r"?\x", r"?\u123", r"?\U00110000", r"?\xFFFFFFFFFFFFFFFF"] {
            assert_eq!(read(source, &cx).unwrap_err(), Error::MalformedUnicdoe(1), "{source}");
        }
        assert_eq!(read(r#""\u123""#, &cx).unwrap_err(), Error::MalformedUnicdoe(1));
        assert_eq!(read("\"\u{e9}\\u123\"", &cx).unwrap_err(), Error::MalformedUnicdoe(3));
    }

    #[test]
    fn m0_reader_unsupported_modifiers_do_not_silently_return_nul() {
        let roots = RootSet::default();
        let cx = Context::new(&roots);
        for source in [r"?\M-a", r"?\H-a", r"?\C-%", r"?\C-"] {
            assert_eq!(read(source, &cx).unwrap_err(), Error::InvalidEscape(1), "{source}");
        }
    }
}
