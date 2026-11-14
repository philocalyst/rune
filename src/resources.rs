//! Explicit installation resource authority, not a guessed GNU installation.
use crate::core::{env::{Env,sym}, gc::{Context,Rt}};
use anyhow::{Result, ensure};
use std::{ffi::OsString, path::PathBuf};
defsym!(DATA_DIRECTORY);

fn selected_directory(explicit: Option<OsString>, emacs: Option<OsString>) -> Result<Option<PathBuf>> {
    let selected = if let Some(path) = explicit {
        ensure!(path.len() <= 65536, "RUNE_DATA_DIRECTORY exceeds path byte limit");
        Some(PathBuf::from(path))
    } else if let Some(paths) = emacs {
        ensure!(paths.len() <= 65536, "EMACSDATA exceeds path byte limit");
        // GNU decode_env_path chooses its first component. Empty components do
        // not invent a cwd/installation fallback in this fail-closed slice.
        Some(std::env::split_paths(&paths).next().unwrap_or_default())
    } else { None };
    let Some(path) = selected else { return Ok(None) };
    ensure!(!path.as_os_str().is_empty() && path.is_absolute(),
        "Resource directory must be an explicit absolute path");
    ensure!(std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
        "Configured resource directory does not exist or is not a directory");
    let _readable = std::fs::read_dir(&path)
        .map_err(|_| anyhow::anyhow!("Configured resource directory is not readable"))?;
    Ok(Some(path))
}

pub(crate) fn capture_data_directory() -> Result<Option<OsString>> {
    let directory = selected_directory(std::env::var_os("RUNE_DATA_DIRECTORY"), std::env::var_os("EMACSDATA"))?;
    let Some(path) = directory else { return Ok(None) };
    let mut path = path.into_os_string();
    if !path.as_encoded_bytes().ends_with(std::path::MAIN_SEPARATOR_STR.as_bytes()) {
        path.push(std::path::MAIN_SEPARATOR_STR);
    }
    ensure!(path.len() <= 65536, "Resource directory with separator exceeds path byte limit");
    Ok(Some(path))
}
pub(crate) fn initialize_data_directory(env: &mut Rt<Env>, cx: &Context) {
    sym::DATA_DIRECTORY.make_special();
    let Some(path) = &env.startup_data_directory else { return };
    let value = match path.to_str() {
        Some(text) => cx.add(text.to_owned()),
        None => cx.add(path.as_encoded_bytes().to_vec()),
    };
    // Env is rooted; shared Context excludes callbacks/GC before publication.
    env.vars.insert(sym::DATA_DIRECTORY, value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{gc::RootSet, env::Env, object::ObjectType};
    use rune_core::macros::root;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
            for _ in 0..100 {
                let path=std::env::temp_dir().join(format!("rune-resources-{}-{}",std::process::id(),NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed)));
                match std::fs::create_dir(&path) { Ok(())=>return Self(path), Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>continue, Err(e)=>panic!("{e}") }
            }
            panic!("no exclusive resource fixture name");
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _=std::fs::remove_dir_all(&self.0); } }
    #[test]
    fn explicit_resource_selection_checks_actual_filesystem_without_guessing() {
        let fixture=Fixture::new();
        assert!(selected_directory(None,None).unwrap().is_none());
        assert!(selected_directory(Some("".into()),None).is_err());
        assert!(selected_directory(Some("relative".into()),None).is_err());
        assert!(selected_directory(Some(fixture.0.join("missing").into_os_string()),None).is_err());
        let file=fixture.0.join("ordinary-file");std::fs::write(&file,b"synthetic").unwrap();
        assert!(selected_directory(Some(file.into_os_string()),None).is_err());
        let paths=std::env::join_paths([&fixture.0,&fixture.0.join("unused-missing")]).unwrap();
        assert_eq!(selected_directory(None,Some(paths)).unwrap(),Some(fixture.0.clone()));
        assert_eq!(selected_directory(Some(fixture.0.clone().into_os_string()),Some("invalid".into())).unwrap(),Some(fixture.0.clone()));
        assert!(selected_directory(Some(OsString::from("x".repeat(65537))),None).is_err());
    }
    #[test]
    #[ignore = "isolated subprocess worker, invoked by resource startup test"]
    fn resource_capture_worker() {
        crate::sym::init_symbols();
        let roots=RootSet::default();let cx=&mut Context::new(&roots);
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        crate::core::env::init_variables(cx,env);
        if let Some(expected)=std::env::var_os("RUNE_EXPECT_RESOURCES") {
            let value=crate::data::symbol_value(sym::DATA_DIRECTORY,env,cx).unwrap();
            root!(value,cx);cx.garbage_collect(true);
            let actual=match value.bind(cx).untag() { ObjectType::String(s)=>s.as_bytes(),ObjectType::ByteString(s)=>&s,_=>panic!("resource path type") };
            assert_eq!(actual,expected.as_encoded_bytes());
            assert!(sym::DATA_DIRECTORY.is_special());
        } else {
            assert!(env.variable_value(sym::DATA_DIRECTORY,cx).unwrap().is_none());
            assert!(crate::data::symbol_value(sym::DATA_DIRECTORY,env,cx).is_err());
        }
    }
    #[test]
    fn configured_startup_is_captured_in_isolated_process_and_preserves_native_bytes() {
        let fixture=Fixture::new();
        let mut cases=vec![None,Some(fixture.0.clone())];
        #[cfg(unix)] { use std::os::unix::ffi::OsStringExt; let raw=fixture.0.join(OsString::from_vec(b"raw-\xff".to_vec()));std::fs::create_dir(&raw).unwrap();cases.push(Some(raw)); }
        for path in cases {
            let mut cmd=std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args(["--ignored","--exact","resources::tests::resource_capture_worker"]).env_remove("RUNE_DATA_DIRECTORY").env_remove("EMACSDATA").env_remove("RUNE_EXPECT_RESOURCES");
            if let Some(path)=path { let mut expected=path.clone().into_os_string();expected.push(std::path::MAIN_SEPARATOR_STR);cmd.env("RUNE_DATA_DIRECTORY",path).env("RUNE_EXPECT_RESOURCES",expected); }
            let output=cmd.output().unwrap();assert!(output.status.success(),"resource worker failed: {}",String::from_utf8_lossy(&output.stderr));
        }
    }
}
