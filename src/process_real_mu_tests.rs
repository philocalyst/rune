//! Real backend + owner pump qualification, NOT a claim of package/Doom loading.
use crate::core::{
    env::{Env, sym},
    gc::{Context, RootSet},
    object::ObjectType,
};
use rune_core::macros::root;

#[test]
#[ignore = "requires explicit official mu binary and isolated synthetic database"]
fn real_mu_owner_pump_preserves_binary_chunks_gc_and_final_sentinel() {
    let binary = std::env::var("RUNE_MU_BINARY").expect("official synthetic mu wrapper");
    let home = std::env::var("RUNE_MU_HOME").expect("synthetic mu database");
    let fixture = std::path::PathBuf::from(std::env::var_os("RUNE_MU_FIXTURE").expect("isolated synthetic fixture root"));
    assert_eq!(std::path::PathBuf::from(&home), fixture.join("muhome"));
    for (name, directory) in [("HOME", "home"), ("XDG_CACHE_HOME", "cache"), ("XDG_CONFIG_HOME", "config")] {
        assert_eq!(std::path::PathBuf::from(std::env::var_os(name).expect("isolated fixture environment")), fixture.join(directory));
    }

    assert!(!binary.contains(['"', '\\', '\n']));
    assert!(!home.contains(['"', '\\', '\n']));
    let source = format!(
        r#"(progn
        (setq mu-pump-bytes "" mu-pump-done nil mu-pump-calls 0 mu-pump-sentinels 0)
        (let ((p (start-process "rune-real-mu-owner" nil "{binary}" "server" "--muhome={home}")) (n 0))
            (unwind-protect
                (progn
                    (set-process-query-on-exit-flag p nil)
                    (set-process-coding-system p 'binary 'utf-8-unix)
                    (set-process-filter p #'(lambda (process chunk)
                        (setq mu-pump-bytes (concat mu-pump-bytes chunk)
                            mu-pump-calls (+ mu-pump-calls 1))
                        (garbage-collect)))
                    (set-process-sentinel p #'(lambda (process event)
                        (setq mu-pump-done event mu-pump-sentinels (+ mu-pump-sentinels 1))
                        (garbage-collect)))
                    (process-send-string p "(ping)\n(find :query \"subject:synthetic\" :threads nil :sortfield :date :descending nil :maxnum 10 :skip-dups t :include-related nil)\n(view :docid 1 :mark-as-read nil :rename nil)\n(quit)\n")
                    (while (and (null mu-pump-done) (< n 300))
                        (setq n (+ n 1)) (accept-process-output p 0.05))
                    (accept-process-output p 0)
                    (list mu-pump-bytes (> (length mu-pump-bytes) 2000)
                        (multibyte-string-p mu-pump-bytes) (stringp mu-pump-bytes)
                        mu-pump-done mu-pump-sentinels (> mu-pump-calls 0)
                        (and (eq (process-status p) 'exit) (= (process-exit-status p) 0))
                        (null (get-process "rune-real-mu-owner"))))
                (delete-process p))))"#
    );
    let roots = &RootSet::default();
    let cx = &mut Context::new(roots);
    sym::init_symbols();
    root!(env, init(Env::new(cx).unwrap()), cx);
    crate::core::env::init_variables(cx, env);
    let object = crate::reader::read(&source, cx).unwrap().0;
    root!(object, cx);
    let (stream, metadata) = {
        let result = crate::interpreter::eval(object, None, env, cx).unwrap();
        let ObjectType::Cons(result) = result.untag() else {
            panic!("expected captured bytes and metadata");
        };
        let raw = result.car();
        let metadata = result.cdr();
        let ObjectType::ByteString(bytes) = raw.untag() else {
            panic!("binary callback stream lost byte storage");
        };
        (bytes.to_vec(), metadata.to_string())
    };
    assert_eq!(metadata, "(t nil t \"finished\n\" 1 t t t)");
    // Verification of returned protocol data only. This does not copy,
    // replace or claim execution of the upstream mu4e Lisp handler.
    let mut offset = 0;
    let mut frames = Vec::new();
    while let Some(relative) = stream[offset..].iter().position(|byte| *byte == 0xfe) {
        let start = offset + relative + 1;
        let end = start + stream[start..].iter().position(|byte| *byte == 0xff).unwrap();
        let digits = &stream[start..end];
        assert!(
            !digits.is_empty() && digits.len() <= 8 && digits.iter().all(u8::is_ascii_hexdigit)
        );
        let length = usize::from_str_radix(std::str::from_utf8(digits).unwrap(), 16).unwrap();
        assert!(length <= 1024 * 1024);
        let payload = &stream[end + 1..end + 1 + length];
        let text = std::str::from_utf8(payload).unwrap();
        let (_, used) = crate::reader::read(text, cx).unwrap();
        assert!(text[used..].trim().is_empty());
        frames.push(text.to_owned());
        offset = end + 1 + length;
    }
    assert_eq!(frames.len(), 5);
    assert!(frames.iter().any(|text| text.contains(":pong") && text.contains("1.12.9")));
    assert!(
        frames
            .iter()
            .any(|text| text.contains(":headers") && text.contains("Rune synthetic λ"))
    );
    assert!(frames.iter().any(|text| text.contains(":found 3")));
    assert!(
        frames
            .iter()
            .any(|text| text.contains(":view") && text.contains("001-synthetic.eml"))
    );
}
