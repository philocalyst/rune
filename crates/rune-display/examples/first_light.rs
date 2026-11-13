//! Produce a real laid-out, clipped first-light frame without a window server.
use rune_display::*;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source=(0..500).map(|i|format!("{i:04}  (defun first-light-{i} (x)\n\t(+ x 42))  ; immutable snapshots → display lists\n")).collect::<String>();
    let snapshot = StringSnapshot::new(source, 1);
    let bounds = Rect { x: 0.0, y: 0.0, width: 900.0, height: 620.0 };
    let mut window = WindowSpec::new(Rect { x: 16.0, y: 16.0, width: 868.0, height: 560.0 });
    window.point = 36;
    window.selection = Some(7..27);
    let spans = [
        FaceSpan {
            range: 0..4,
            face: Face { foreground: Color(130, 150, 180, 255), ..Face::default() },
        },
        FaceSpan {
            range: 7..12,
            face: Face { foreground: Color(199, 146, 234, 255), ..Face::default() },
        },
    ];
    let metrics = FixedMetrics::default();
    let frame = FrameDisplay {
        protocol_version: DISPLAY_PROTOCOL_VERSION,
        generation: 1,
        bounds,
        background: Face::default().background,
        windows: vec![layout_window(&snapshot, &window, &spans, &metrics)?],
    };
    let mut target = svg::SvgTarget::default();
    paint_frame(&frame, metrics.0, &mut target);
    let destination = std::env::args().nth(1).unwrap_or_else(|| "first-light.svg".into());
    std::fs::write(&destination, target.finish(bounds))?;
    eprintln!(
        "Wrote {destination}; {} visible+overscan rows; {} source characters scanned",
        frame.windows[0].rows.len(),
        frame.windows[0].scanned_chars
    );
    Ok(())
}
