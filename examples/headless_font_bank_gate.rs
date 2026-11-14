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
            let bank = Rc::new(NativeFontVariantBank::measure(&base, cx)?);
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
            // Exercise the same prevalidated per-face admission used by the
            // opt-in FrameView path, without constructing a Window or painter.
            let rect=rune_display::Rect{x:0.,y:0.,width:100.,height:40.};
            let row=std::sync::Arc::new(rune_display::DisplayRow{source:0..1,bounds:rect,baseline:metrics.ascent,continued:false,truncated:false,clusters:vec![rune_display::Cluster{source:0..1,bounds:rect,text:"x".into(),face:rune_display::Face{id:5,..rune_display::Face::default()}}]});
            let frame=std::sync::Arc::new(rune_display::FrameDisplay{protocol_version:rune_display::DISPLAY_PROTOCOL_VERSION,generation:10,bounds:rect,background:rune_display::Face::default().background,windows:vec![rune_display::WindowDisplay{window_id:1,buffer_id:1,revision:1,viewport:rect,background:rune_display::Face::default().background,rows:vec![row],selection:vec![],cursor:None,next_anchor:1,scanned_chars:1,budget_exhausted:false}]});
            for variant in [NativeFontVariant::Normal,NativeFontVariant::Bold,NativeFontVariant::Italic,NativeFontVariant::BoldItalic] {
                let request=rune_display::FrameFontPlan::new(frame.clone(),id,metrics,vec![rune_display::FrameFontAssignment{face_id:5,variant}])?;
                let prepared=rune_gpui::NativeFrameFontPlan::new(request,&base,bank.clone(),cx);
                if capabilities.supports_variant(id,metrics,variant) {
                    let prepared=prepared?;
                    if !prepared.matches_frame(&frame) || prepared.matches_frame(&std::sync::Arc::new((*frame).clone())) {return Err("frame plan identity mismatch")}
                    if prepared.font_for_face(5,cx)?!=bank.get_compatible_font(id,metrics,variant,cx)? {return Err("face plan selected wrong measured descriptor")}
                    println!("HEADLESS_FRAME_PLAN_ADMITTED {variant:?}");
                } else if prepared.is_ok(){return Err("unavailable face plan admitted")}
            }
            for (request_id,request_metrics) in [(stale_id,metrics),(id,changed)] {
                let request=rune_display::FrameFontPlan::new(frame.clone(),request_id,request_metrics,vec![])?;
                if rune_gpui::NativeFrameFontPlan::new(request,&base,bank.clone(),cx).is_ok(){return Err("stale frame font plan admitted")}
            }
            let other_base=NativeFont::measure(font("DejaVu Sans Mono"),18.,3.,cx)?;
            let request=rune_display::FrameFontPlan::new(frame.clone(),id,metrics,vec![])?;
            if rune_gpui::NativeFrameFontPlan::new(request,&other_base,bank.clone(),cx).is_ok(){return Err("foreign view baseline admitted")}
            println!("HEADLESS_FRAME_PLAN_GATE_PASS equal_data_arc_not_matched=true stale_bank_rejected=true changed_metrics_rejected=true foreign_baseline_rejected=true");
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
