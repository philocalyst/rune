//! Real native font admission diagnostic. No Window, GPU paint or shaping.
use gpui::{Application, font};
use rune_gpui::{NativeFont, NativeFontVariant, NativeFontVariantBank};
use std::{cell::Cell, process::ExitCode, rc::Rc};

fn main() -> ExitCode {
    let outcome = Rc::new(Cell::new(2u8));
    let result = outcome.clone();
    Application::headless().run(move |cx| {
        let measured = (|| -> Result<(), &'static str> {
            let base = NativeFont::measure(font("DejaVu Sans Mono"), 17., 3., cx)?;
            let bank = NativeFontVariantBank::measure(&base, cx)?;
            let capabilities = bank.capabilities(cx)?;
            let id = capabilities.bank_id;
            let metrics = base.metrics();
            bank.get_compatible_font(id, metrics, NativeFontVariant::Normal, cx)?;
            let stale_id = id.checked_add(1).ok_or("bank ID probe overflow")?;
            if bank.get_compatible_font(stale_id, metrics, NativeFontVariant::Normal, cx).is_ok() {
                return Err("stale bank identity admitted");
            }
            let mut changed = metrics;
            changed.advance += 1.;
            if bank.get_compatible_font(id, changed, NativeFontVariant::Normal, cx).is_ok() {
                return Err("changed baseline geometry admitted");
            }
            let mut admitted = 0;
            for variant in [NativeFontVariant::Normal, NativeFontVariant::Bold, NativeFontVariant::Italic, NativeFontVariant::BoldItalic] {
                let actual = bank.get_compatible_font(id, metrics, variant, cx);
                if actual.is_ok() != capabilities.supports_variant(id, metrics, variant) {
                    return Err("measured capability disagrees with live font gate");
                }
                match actual {
                    Ok(descriptor) => {
                        let resolved = cx.text_system().resolve_font(descriptor);
                        admitted += 1;
                        println!("HEADLESS_BANK_ADMITTED {variant:?} {resolved:?}");
                    }
                    Err(error) => println!("HEADLESS_BANK_UNAVAILABLE {variant:?} {error}"),
                }
            }
            println!("HEADLESS_BANK_GATE_PASS admitted={admitted} stale_id_rejected=true changed_metrics_rejected=true");
            Ok(())
        })();
        match measured {
            Ok(()) => result.set(0),
            Err(error) => { eprintln!("HEADLESS_BANK_GATE_ERROR {error}"); result.set(1); }
        }
        rune_gpui::quit_on_next_foreground_turn(cx);
    });
    ExitCode::from(outcome.get())
}
