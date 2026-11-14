//! Native shell selection captured once per Env, not a live host-environment fallback.
use crate::core::{env::Env, gc::{Context, Rt}, object::Object};
defvar!(SHELL_FILE_NAME, crate::shell_startup::initial_shell_file_name(env, cx));
pub(crate) fn initial_shell_file_name<'a>(env: &Rt<Env>, cx: &'a Context) -> Object<'a> {
    let shell = &env.startup_shell_file_name;
    match shell.to_str() {
        Some(s) => cx.add(s.to_owned()),
        None => cx.add(shell.as_encoded_bytes().to_vec()),
    }
}
#[cfg(test)]
mod tests {
    use crate::core::{env::{Env,sym}, gc::{Context,RootSet}, object::ObjectType};
    use rune_core::macros::root;
    #[test]
    #[ignore = "isolated child entry; run through startup_shell_capture_uses_isolated_native_environment"]
    fn startup_capture_worker() {
        let expected=std::env::var_os("RUNE_EXPECT_SHELL").unwrap();
        let roots=RootSet::default();let cx=&mut Context::new(&roots);
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        crate::core::env::init_variables(cx,env);
        let value=crate::data::symbol_value(sym::SHELL_FILE_NAME,env,cx).unwrap();
        let bytes=match value.untag() { ObjectType::String(s)=>s.as_bytes(), ObjectType::ByteString(s)=>&s,_=>panic!("shell-file-name is not a String") };
        assert_eq!(bytes,expected.as_encoded_bytes());
        assert_eq!(env.startup_shell_file_name,expected);
    }
    #[test]
    fn startup_shell_capture_uses_isolated_native_environment() {
        use std::ffi::OsString;
        let mut cases=vec![(None,OsString::from("/bin/sh")),(Some(OsString::from("")),OsString::from("")),(Some(OsString::from("relative")),OsString::from("relative")),(Some(OsString::from("/bin/false")),OsString::from("/bin/false"))];
        #[cfg(unix)] { use std::os::unix::ffi::OsStringExt; let raw=OsString::from_vec(b"/synthetic/\xff\xfe".to_vec());cases.push((Some(raw.clone()),raw)); }
        for (shell,expected) in cases {
            let mut cmd=std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args(["--ignored","--exact","shell_startup::tests::startup_capture_worker"]).env_remove("SHELL").env("RUNE_EXPECT_SHELL",expected).env("ESHELL","/bin/true");
            if let Some(shell)=shell { cmd.env("SHELL",shell); }
            let out=cmd.output().unwrap(); assert!(out.status.success(),"isolated shell worker failed: {}",String::from_utf8_lossy(&out.stderr));
        }
    }
}
