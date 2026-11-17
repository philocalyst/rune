// Parse the enclosing Rust item and only its own attribute token tree. Never
// search subsequent source text for a name: whitespace, comments, string
// contents and adjacent attributes must not alter another function's identity.
pub(crate) fn scan_defuns(contents: &str) -> syn::Result<Vec<(String, String)>> {
    let file = syn::parse_file(contents)?;
    let mut functions = Vec::new();
    for item in file.items {
        let syn::Item::Fn(function) = item else { continue };
        let mut definition = None;
        for attribute in &function.attrs {
            if !attribute.path().is_ident("defun") { continue; }
            if definition.is_some() {
                return Err(syn::Error::new_spanned(attribute, "Duplicate defun attribute"));
            }
            let mut name = None;
            let mut required = None;
            match &attribute.meta {
                syn::Meta::Path(_) => {},
                syn::Meta::List(_) => attribute.parse_nested_meta(|meta| {
                    if meta.path.is_ident("name") {
                        if name.is_some() { return Err(meta.error("Duplicate defun name")); }
                        name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                    } else if meta.path.is_ident("required") {
                        if required.is_some() { return Err(meta.error("Duplicate defun required")); }
                        required = Some(meta.value()?.parse::<syn::LitInt>()?.base10_parse::<u16>()?);
                    } else {
                        return Err(meta.error("Unknown defun option"));
                    }
                    Ok(())
                })?,
                _ => return Err(syn::Error::new_spanned(attribute, "Invalid defun attribute")),
            }
            let rust_name = function.sig.ident.to_string();
            definition = Some((rust_name.clone(), name.unwrap_or_else(|| rust_name.replace('_', "-"))));
        }
        if let Some(definition) = definition { functions.push(definition); }
    }
    Ok(functions)
}

#[cfg(test)]
#[path = "registration_corpus.rs"]
mod registration_corpus;

#[cfg(test)]
mod tests {
    use super::scan_defuns;

    #[test]
    fn whitespace_and_option_order_preserve_names() {
        for attribute in [r#"#[defun(name="expand-file-name")]"#,
                          r#"#[defun(name = "expand-file-name")]"#,
                          "#[defun(\n name = \"expand-file-name\",\n)]",
                          r#"#[defun(required = 1, name /* comment */ = "expand-file-name",)]"#] {
            let source = format!("{attribute}\nfn expand_dispatch() {{}}");
            assert_eq!(scan_defuns(&source).unwrap(), vec![("expand_dispatch".into(), "expand-file-name".into())]);
        }
    }

    #[test]
    fn adjacent_attributes_and_body_literals_cannot_supply_another_name() {
        let source = r##"
            #[defun(name="expand-file-name")]
            #[allow(dead_code)]
            fn expand_dispatch() { let decoy = "name = \"body-decoy\""; }
            #[defun(name = "write-region")]
            fn write_region_lisp() {}
        "##;
        assert_eq!(scan_defuns(source).unwrap(), vec![("expand_dispatch".into(), "expand-file-name".into()), ("write_region_lisp".into(), "write-region".into())]);
    }

    #[test]
    fn default_names_comments_and_nested_test_items_are_scoped() {
        let source = r##"
            // #[defun(name = "comment-decoy")]
            #[defun] fn ordinary_name() {}
            #[defun(required=0)] fn another_name() {}
            mod tests { #[defun(name="nested-decoy")] fn nested() {} }
        "##;
        assert_eq!(scan_defuns(source).unwrap(), vec![("ordinary_name".into(), "ordinary-name".into()), ("another_name".into(), "another-name".into())]);
    }

    #[test]
    fn malformed_metadata_never_borrows_a_later_attribute() {
        for attribute in [r#"#[defun(name=3)]"#, r#"#[defun(name)]"#,
                          r#"#[defun(name="a",name="b")]"#, r#"#[defun(unknown=1)]"#,
                          r#"#[defun(required=65536)]"#, r#"#[defun(required=1,required=2)]"#,
                          r#"#[defun="bad"]"#, r#"#[defun] #[defun]"#] {
            let source = format!("{attribute} fn first() {{}}\n#[defun(name = \"later\")] fn second() {{}}");
            assert!(scan_defuns(&source).is_err(), "accepted {attribute}");
        }
    }

    #[test]
    fn current_repository_registration_corpus_matches_reviewed_pairs() {
        for (source_name, source, expected) in super::registration_corpus::CASES {
            let actual = scan_defuns(source).unwrap_or_else(|error| panic!("{source_name}: {error}"));
            let actual: Vec<(&str, &str)> = actual.iter().map(|(rust, lisp)| (rust.as_str(), lisp.as_str())).collect();
            assert_eq!(actual.as_slice(), *expected, "registration drift in {source_name}");
        }
    }
}
