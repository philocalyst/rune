//! Intentional startup-failure lifecycle probe. No Window or GPU paint.
use gpui::Application;
use std::{process::ExitCode,sync::{Arc,atomic::{AtomicBool,Ordering}}};
fn main()->ExitCode{
    let failed=Arc::new(AtomicBool::new(false));let reported=failed.clone();
    Application::headless().run(move|cx|{
        // Same ordering as native startup error branches: record failure first,
        // queue official foreground quit, return without opening a window.
        reported.store(true,Ordering::Relaxed);
        println!("HEADLESS_STARTUP_FAILURE_RECORDED");
        rune_gpui::quit_on_next_foreground_turn(cx);
    });
    if !failed.load(Ordering::Relaxed){eprintln!("startup failure state lost");return ExitCode::FAILURE}
    println!("HEADLESS_STARTUP_FAILURE_EXIT_VERIFIED");
    // A passing diagnostic verifies an intentional failure returns. Production
    // native::run still maps its recorded failure to Err(()), not success.
    ExitCode::SUCCESS
}
