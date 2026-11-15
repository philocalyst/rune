//! Actual native preflight only; no Window or ShapedLine::shape call executes.
use gpui::{Application, font};
use rune_display::retained_row_transaction::{RetainedRowScope, RetainedRowTransaction};
use rune_gpui::{NativeFont, NativeFontVariantBank, retained_row_stage::preflight_retained_row};
use std::{cell::Cell, process::ExitCode, rc::Rc};

fn run(app: &gpui::App) -> Result<(), &'static str> {
    let baseline = NativeFont::measure(font("DejaVu Sans Mono"), 17.0, 3.0, app)?;
    let bank = NativeFontVariantBank::measure(&baseline, app)?;
    let capabilities = bank.capabilities(app)?;
    let scope = RetainedRowScope {
        sequence: 1, window: 1, buffer: 1, text_epoch: 1, source_start_scalar: 0,
        style_revision: 1, viewport_revision: 1,
        font_bank_generation: capabilities.bank_id, shaping_policy_revision: 1,
    };
    let mut transaction = RetainedRowTransaction::new().map_err(|_| "request construction")?;
    let request = transaction.begin(scope, "AVATAR").map_err(|_| "request admission")?;
    preflight_retained_row(&request, &baseline, &bank, app)?;
    transaction.cancel();
    if preflight_retained_row(&request, &baseline, &bank, app).is_ok() {
        return Err("canceled pending request admitted");
    }
    let request = transaction.begin(scope, "AVATAR").map_err(|_| "live bank test request")?;
    let other_bank = NativeFontVariantBank::measure(&baseline, app)?;
    if preflight_retained_row(&request, &baseline, &other_bank, app).is_ok() {
        return Err("stale bank admitted");
    }
    transaction.cancel();
    for (start, policy) in [(1, 1), (0, 2)] {
        let mut unsupported = scope;
        unsupported.source_start_scalar = start;
        unsupported.shaping_policy_revision = policy;
        let request = transaction.begin(unsupported, "AVATAR").map_err(|_| "test request")?;
        if preflight_retained_row(&request, &baseline, &bank, app).is_ok() {
            return Err("unsupported policy admitted");
        }
        transaction.cancel();
    }
    println!("HEADLESS_ROW_STAGE_PREFLIGHT_PASS current_bank=true canceled_request_rejected=true other_bank_rejected=true partial_source_rejected=true unknown_policy_rejected=true actual_shape=false window=false paint=false");
    Ok(())
}

fn main() -> ExitCode {
    let status = Rc::new(Cell::new(2));
    let outcome = status.clone();
    Application::headless().run(move |app| {
        match run(app) {
            Ok(()) => outcome.set(0),
            Err(error) => { eprintln!("HEADLESS_ROW_STAGE_PREFLIGHT_ERROR {error}"); outcome.set(1); }
        }
        rune_gpui::quit_on_next_foreground_turn(app);
    });
    ExitCode::from(status.get())
}
