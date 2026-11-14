use std::ffi::OsStr;
use std::io::Write as _;
use std::path::MAIN_SEPARATOR;
use std::{
    fs::{self, File},
    path::Path,
};

// take an input str and parse it with syn::ExprCall and return the args as strings
#[must_use]
pub fn parse_args(input: &str) -> Vec<String> {
    use quote::ToTokens;
    let pseudo_fn = format!("X{input}");
    let expr = syn::parse_str::<syn::ExprCall>(&pseudo_fn).unwrap();
    expr.args.into_iter().map(|e| e.to_token_stream().to_string()).collect()
}

// get the substring between two string
fn get_substring_between<'a>(contents: &'a str, start: &str, end: &str) -> &'a str {
    let start_idx = contents.find(start).unwrap() + start.len();
    let end_idx = contents[start_idx..].find(end).unwrap() + start_idx;
    &contents[start_idx..end_idx]
}

// get substring between string and predicate function
fn get_substring_between_predicate<'a, F>(contents: &'a str, start: &str, predicate: F) -> &'a str
where
    F: Fn(char) -> bool,
{
    let start_idx = contents.find(start).unwrap() + start.len();
    let end_idx = contents[start_idx..].find(predicate).unwrap() + start_idx;
    &contents[start_idx..end_idx]
}

fn map_varname(ident: &str) -> String {
    if let Some(stripped) = ident.strip_prefix("KW_") {
        convert(':', stripped)
    } else {
        convert("", ident)
    }
}

fn convert(lisp_name: impl Into<String>, rust_name: &str) -> String {
    let mut lisp_name = lisp_name.into();
    lisp_name.extend(rust_name.chars().map(|c| match c {
        '_' => '-',
        c => c.to_ascii_lowercase(),
    }));
    lisp_name
}

#[derive(PartialEq)]
enum DefvarType {
    Bool,
    Other,
}

#[expect(clippy::too_many_lines)]
fn main() {
    let mut all_defun = Vec::new();
    let mut all_defvar = Vec::new();
    let mut all_defsym = Vec::new();

    // rerun for all top level files
    for entry in fs::read_dir("src").unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(OsStr::to_str) == Some("rs") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    for entry in fs::read_dir("src").unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            if path.extension().and_then(OsStr::to_str) != Some("rs") {
                continue;
            }
            let contents = fs::read_to_string(&path).unwrap();
            for (start, end) in contents.match_indices("\n#[defun") {
                let non_symbol = |c: char| !(c.is_alphanumeric() || c == '_');
                let name = get_substring_between_predicate(&contents[start..], "fn ", non_symbol);

                // check if the substring immdiately after end is name
                let rest = &contents[(start + end.len())..];
                let lisp_name = if rest.starts_with("(name") {
                    get_substring_between(rest, "name = \"", "\"").to_string()
                } else {
                    name.replace('_', "-")
                };
                // convert the path name to a hierarchy name with slashes replaced with colons
                let struct_name = {
                    let basename = path.strip_prefix("src/").unwrap().file_stem().unwrap();
                    let import_path = basename.to_str().unwrap().replace(MAIN_SEPARATOR, ":");
                    format!("crate::{import_path}::__subr_{name}")
                };
                all_defun.push((struct_name, name.to_string(), lisp_name));
            }
            // process all strings starting with defvar
            for (start, _) in contents.match_indices("\ndefvar") {
                let defvar_type = if contents[start..].starts_with("\ndefvar_bool!") {
                    DefvarType::Bool
                } else if contents[start..].starts_with("\ndefvar!") {
                    DefvarType::Other
                } else {
                    continue;
                };
                let body = get_substring_between(&contents[start..], "!", ";");
                let args = parse_args(body);
                let len = args.len();
                let mut fields = args.into_iter();
                let ident = fields.next().unwrap();
                let (ident, name, value) = match len {
                    1 => {
                        let name = map_varname(&ident);
                        (ident, name, None)
                    }
                    2 => {
                        let name = map_varname(&ident);
                        let value = fields.next().unwrap();
                        (ident, name, Some(value))
                    }
                    3 => {
                        let name = fields.next().unwrap();
                        let value = fields.next().unwrap();
                        (ident, name, Some(value))
                    }
                    _ => panic!("defvar form was too long {path:?}"),
                };
                all_defvar.push((ident, name, value, defvar_type));
            }

            // process all strings starting with defsym
            for (start, _) in contents.match_indices("\ndefsym!") {
                let body = get_substring_between(&contents[start..], "defsym!", ";");
                let args = parse_args(body);
                let mut fields = args.into_iter();
                let name = fields.next().unwrap();
                let value = fields.next();
                all_defsym.push((name, value));
            }
        }
    }

    let out_dir = std::env::var("OUT_DIR").unwrap();
    // println!("cargo:warning={out_dir}/sym.rs");
    let dest_path = Path::new(&out_dir).join("sym.rs");
    let mut f = File::create(dest_path).unwrap();

    // One Lisp identity may have independent value/function declarations.
    let mut symbols: Vec<(String, bool)> = vec![("nil".into(), false), ("t".into(), false)];
    let mut aliases = std::collections::BTreeMap::<String, usize>::new();
    aliases.insert("NIL".into(), 0);
    aliases.insert("TRUE".into(), 1);
    let mut declare = |rust: &str, lisp: String, special: bool| {
        let index = match symbols.iter().position(|entry| entry.0 == lisp) {
            Some(index) => {
                symbols[index].1 |= special;
                index
            }
            None => {
                let index = symbols.len();
                symbols.push((lisp, special));
                index
            }
        };
        let rust = rust.to_ascii_uppercase();
        if let Some(previous) = aliases.insert(rust.clone(), index) {
            assert_eq!(previous, index, "Rust symbol alias {rust} names different Lisp symbols");
        }
        index
    };
    for (rust, lisp) in &all_defsym {
        declare(
            rust,
            lisp.as_ref()
                .map_or_else(|| map_varname(rust), |name| name.trim_matches('"').to_owned()),
            false,
        );
    }
    for (rust, lisp, _, _) in &all_defvar {
        declare(rust, lisp.clone(), true);
    }
    let subr_indices: Vec<_> = all_defun
        .iter()
        .map(|(_, rust, lisp)| declare(rust, lisp.clone(), false))
        .collect();
    let symbol_len = symbols.len();
    writeln!(
        f,
        "
#[allow(non_upper_case_globals)]
#[allow(non_snake_case)]
#[allow(dead_code)]
pub(crate) mod sym {{
use crate::core::object::SymbolCell;
use crate::core::object::Symbol;

pub(in crate::core) static BUILTIN_SYMBOLS: [SymbolCell; {symbol_len}] = [
    SymbolCell::new_static_const(\"nil\"),
    SymbolCell::new_static_const(\"t\"),",
    )
    .unwrap();

    for (name, special) in symbols.iter().skip(2) {
        let constructor = if *special { "new_static_special" } else { "new_static" };
        writeln!(f, "    SymbolCell::{constructor}({name:?}),").unwrap();
    }
    writeln!(f, "];\n").unwrap();
    for (alias, index) in aliases {
        writeln!(f, "pub(crate) const {alias}: Symbol = Symbol::new_builtin({index});").unwrap();
    }

    // all SubrFn
    let subr_len = all_defun.len();
    writeln!(f, "pub(crate) static SUBR_DEFS: [&crate::core::object::SubrFn; {subr_len}] = [",)
        .unwrap();
    for (subr_name, _, _) in &all_defun {
        writeln!(f, "    &{subr_name},",).unwrap();
    }
    // End SUBR_DEFS
    writeln!(f, "];\n").unwrap();

    writeln!(f, "const SUBR_SYMBOL_INDICES: [usize; {subr_len}] = {subr_indices:?};").unwrap();
    writeln!(
        f,
        "
pub(crate) fn init_symbols() {{
    for (index, func) in SUBR_SYMBOL_INDICES.iter().zip(SUBR_DEFS.iter()) {{
        let sym = &BUILTIN_SYMBOLS[*index];
        unsafe {{ sym.set_func((*func).into()).unwrap(); }}
    }}
}}
"
    )
    .unwrap();
    writeln!(
        f,
        r#"
#[cfg(test)]
mod generated_symbol_manifest_tests {{
    use super::*;
    #[test]
    fn canonical_names_are_unique_and_subr_indices_match_names() {{
        let mut names = std::collections::BTreeSet::new();
        for index in 0..BUILTIN_SYMBOLS.len() {{
            assert!(names.insert(Symbol::new_builtin(index).get().name()));
        }}
        let mut subr_names = std::collections::BTreeSet::new();
        for (index, function) in SUBR_SYMBOL_INDICES.iter().zip(SUBR_DEFS.iter()) {{
            assert!(subr_names.insert(function.name), "duplicate native Lisp function");
            assert_eq!(Symbol::new_builtin(*index).get().name(), function.name);
        }}
    }}
}}
"#
    )
    .unwrap();
    // End mod sym
    writeln!(f, "}}").unwrap();

    writeln!(
        f,
        "
use std::sync::LazyLock;
pub(crate) static INTERNED_SYMBOLS: LazyLock<std::sync::Mutex<SymbolMap>> = LazyLock::new(|| std::sync::Mutex::new({{
        let size: usize = {symbol_len};
        let mut map = SymbolMapCore::with_capacity(size);
        for sym in &sym::BUILTIN_SYMBOLS {{
            map.pre_init(unsafe {{Symbol::from_ptr(sym as *const _)}});
        }}
        SymbolMap {{
            map,
            block: Block::new_global(),
        }}
    }}));
"
    )
    .unwrap();

    writeln!(
        f,
        "
#[allow(unused_qualifications)]
pub(crate) fn init_variables(
    cx: &crate::core::gc::Context,
    env: &mut crate::core::gc::Rt<crate::core::env::Env>,
) {{
use crate::core::object;
use rune_core::macros::list;
env.initialize_condition_metadata(cx);
"
    )
    .unwrap();

    let mut bool_vars = Vec::new();

    // write out the value of each defvar
    for (ident, _, value, ty) in all_defvar {
        // handled below
        if ident == "BYTE_BOOLEAN_VARS" {
            continue;
        }
        let mut value = value.unwrap_or_else(|| "object::NIL".to_string());

        // if value starts with list! then insert cx before that last character
        if value.starts_with("list") {
            let len = value.len();
            value.insert_str(len - 1, "; cx");
        }
        writeln!(f, "env.vars.insert(sym::{ident}, cx.add({value}));").unwrap();
        if DefvarType::Bool == ty {
            bool_vars.push(ident);
        }
    }

    // write out the boolean vars
    writeln!(f, "let bool_vars = list![").unwrap();
    for ident in bool_vars {
        writeln!(f, "    sym::{ident},").unwrap();
    }
    writeln!(f, "; cx];").unwrap();
    writeln!(f, "env.vars.insert(sym::BYTE_BOOLEAN_VARS, bool_vars);").unwrap();

    writeln!(f, "}}").unwrap();
}
