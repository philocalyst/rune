use rune_display::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::sync_channel,
};
fn spec(width: f32, height: f32) -> WindowSpec {
    let mut s = WindowSpec::new(Rect { x: 0.0, y: 0.0, width, height });
    s.overscan_rows = 0;
    s
}
fn frame(window: WindowDisplay) -> FrameDisplay {
    FrameDisplay {
        protocol_version: DISPLAY_PROTOCOL_VERSION,
        generation: 1,
        bounds: window.viewport,
        background: Face::default().background,
        windows: vec![window],
    }
}
#[test]
fn unicode_positions_wrap_tabs_and_cursor_share_geometry() {
    let text = StringSnapshot::new("aé\tZ\nlast", 7);
    let mut s = spec(40.0, 100.0);
    s.tab_width = 4;
    s.point = 3;
    s.selection = Some(1..3);
    let display = layout_window(&text, &s, &[], &FixedMetrics::default()).unwrap();
    assert_eq!(
        display.rows[0].clusters.iter().map(|c| c.text.as_ref()).collect::<String>(),
        "aé  Z"
    );
    assert_eq!(display.rows[0].source, 0..4);
    assert_eq!(display.cursor.as_ref().unwrap().bounds.x, 32.0);
    assert_eq!(display.selection.len(), 2);
    let f = frame(display);
    let hit = f.hit_test(Point { x: 9.0, y: 5.0 }).unwrap();
    assert_eq!(hit.position, 1);
    assert_eq!(hit.revision, 7);
    let hit = f.hit_test(Point { x: 25.0, y: 5.0 }).unwrap();
    assert_eq!(hit.position, 3);
}
#[test]
fn wrapping_preserves_unconsumed_scalar_and_newline_endpoint() {
    let text = StringSnapshot::new("abc\né", 1);
    let mut s = spec(16.0, 100.0);
    s.point = 2;
    let d = layout_window(&text, &s, &[], &FixedMetrics::default()).unwrap();
    assert_eq!(
        d.rows.iter().map(|r| r.source.clone()).collect::<Vec<_>>(),
        vec![0..2, 2..3, 4..5]
    );
    assert!(!d.rows[0].continued);
    assert!(d.rows[1].continued);
    assert!(!d.rows[2].continued);
    assert_eq!(d.cursor.unwrap().bounds.y, 20.0);
}
#[test]
fn truncation_uses_newline_index_not_long_line_scan() {
    let source = "x".repeat(1_000_000) + "\nend";
    let text = StringSnapshot::new(source, 1);
    let mut s = spec(32.0, 40.0);
    s.wrap = false;
    s.scan_budget = 20;
    let d = layout_window(&text, &s, &[], &FixedMetrics::default()).unwrap();
    assert_eq!(d.rows.len(), 2);
    assert!(d.rows[0].truncated);
    assert_eq!(d.rows[1].source, 1_000_001..1_000_004);
    assert!(d.scanned_chars <= 8);
    assert!(!d.budget_exhausted);
}
#[test]
fn budget_stops_pathological_zero_width_and_preserves_resume_anchor() {
    let t = StringSnapshot::new("a".repeat(1000), 1);
    let mut s = spec(8000.0, 40.0);
    s.scan_budget = 7;
    let d = layout_window(&t, &s, &[], &FixedMetrics::default()).unwrap();
    assert_eq!(d.scanned_chars, 7);
    assert_eq!(d.next_anchor, 7);
    assert!(d.budget_exhausted);
}
#[test]
fn empty_trailing_newline_and_exact_budget_produce_eof_cursor() {
    for text in ["", "\n"] {
        let t = StringSnapshot::new(text, 1);
        let mut s = spec(80.0, 80.0);
        s.point = text.chars().count();
        s.scan_budget = 1;
        let d = layout_window(&t, &s, &[], &FixedMetrics::default()).unwrap();
        assert!(d.cursor.is_some());
    }
}
#[test]
fn indexed_snapshot_seeks_across_multibyte_checkpoints() {
    let t = StringSnapshot::new("é".repeat(1000) + "\nlast", 1);
    assert_eq!(t.chars_from(999).take(3).collect::<String>(), "é\nl");
    assert_eq!(t.next_line_start(999), Some(1001));
    assert_eq!(t.next_line_start(1001), None);
}
#[test]
fn invalid_input_cannot_emit_nan_or_invalid_ranges() {
    let t = StringSnapshot::new("abc", 1);
    let mut s = spec(f32::NAN, 40.0);
    assert!(layout_window(&t, &s, &[], &FixedMetrics::default()).is_err());
    s = spec(80.0, 40.0);
    s.anchor = 4;
    assert!(layout_window(&t, &s, &[], &FixedMetrics::default()).is_err());
    s.anchor = 0;
    s.tab_width = 0;
    assert!(layout_window(&t, &s, &[], &FixedMetrics::default()).is_err());
}
#[test]
fn resolved_face_spans_survive_wrap_and_svg_escapes_content() {
    let t = StringSnapshot::new("<&>é", 1);
    let s = spec(80.0, 40.0);
    let mut face = Face::default();
    face.foreground = Color(255, 0, 0, 255);
    let d =
        layout_window(&t, &s, &[FaceSpan { range: 1..3, face }], &FixedMetrics::default()).unwrap();
    assert_eq!(d.rows[0].clusters[1].face, face);
    let f = frame(d);
    let mut target = svg::SvgTarget::default();
    paint_frame(&f, FixedMetrics::default().0, &mut target);
    let svg = target.finish(f.bounds);
    assert!(svg.contains("&lt;"));
    assert!(svg.contains("&amp;"));
    assert!(!svg.contains("<text x=\"8\" y=\"13\" fill=\"#ff0000\" font-family=\"monospace\" font-size=\"17\" xml:space=\"preserve\">&</text>"));
}
#[test]
fn latest_mailbox_rejects_stale_frames_and_quit_precedes_queue_delivery() {
    let mailbox = DisplayMailbox::default();
    let t = StringSnapshot::new("", 1);
    let f = Arc::new(frame(
        layout_window(&t, &spec(80.0, 40.0), &[], &FixedMetrics::default()).unwrap(),
    ));
    mailbox.publish(f.clone()).unwrap();
    assert!(mailbox.publish(f).is_err());
    assert_eq!(mailbox.latest().unwrap().generation, 1);
    let (tx, _rx) = sync_channel(0);
    let quit = Arc::new(AtomicBool::new(false));
    let sender = InputSender::new(tx, quit.clone());
    assert!(
        sender
            .send(InputEvent::Key {
                key: "g".into(),
                text: None,
                control: true,
                meta: false,
                shift: false
            })
            .is_err()
    );
    assert!(quit.load(Ordering::Acquire));
}
#[test]
fn unchanged_rows_are_shared_across_cursor_only_frames() {
    let text = StringSnapshot::new("hello\nworld", 2);
    let mut s = spec(80.0, 60.0);
    let mut cache = LayoutCache::new(2);
    let first = cache.layout(&text, &s, &[], &FixedMetrics::default()).unwrap();
    s.point = 3;
    let next = cache.layout(&text, &s, &[], &FixedMetrics::default()).unwrap();
    assert!(Arc::ptr_eq(&first.rows[0], &next.rows[0]));
    assert_ne!(first.cursor, next.cursor);
    let changed = StringSnapshot::new("hello\nWORLD", 3);
    let third = cache.layout(&changed, &s, &[], &FixedMetrics::default()).unwrap();
    assert!(Arc::ptr_eq(&next.rows[0], &third.rows[0]));
    assert!(!Arc::ptr_eq(&next.rows[1], &third.rows[1]));
}
#[test]
fn a_hundred_megabyte_lazy_document_does_not_require_materialization() {
    struct HundredMb;
    impl TextSnapshot for HundredMb {
        fn revision(&self) -> u64 {
            1
        }
        fn len_chars(&self) -> usize {
            100_000_005
        }
        fn chars_from(&self, start: usize) -> Box<dyn Iterator<Item = char> + '_> {
            Box::new((start..self.len_chars()).map(|p| if p == 100_000_000 { '\n' } else { 'x' }))
        }
        fn next_line_start(&self, start: usize) -> Option<usize> {
            (start < 100_000_001).then_some(100_000_001)
        }
    }
    let mut s = spec(32.0, 40.0);
    s.wrap = false;
    let d = layout_window(&HundredMb, &s, &[], &FixedMetrics::default()).unwrap();
    assert_eq!(d.rows[1].source.start, 100_000_001);
    assert!(d.scanned_chars <= 8);
    let f = frame(d);
    assert!(f.hit_test(Point { x: -1.0, y: 3.0 }).is_none());
}

#[test]
fn window_background_covers_empty_viewport_and_row_remainder() {
    let snapshot = StringSnapshot::new("x", 1);
    let mut spec = WindowSpec::new(Rect { x: 5.0, y: 7.0, width: 80.0, height: 60.0 });
    spec.default_face.background = Color(12, 34, 56, 255);
    let metrics = FixedMetrics::default();
    let window = layout_window(&snapshot, &spec, &[], &metrics).unwrap();
    assert_eq!(window.background, spec.default_face.background);
    let frame = FrameDisplay {
        protocol_version: DISPLAY_PROTOCOL_VERSION,
        generation: 1,
        bounds: Rect { x: 0.0, y: 0.0, width: 100.0, height: 100.0 },
        background: Color(0, 0, 0, 255),
        windows: vec![window],
    };
    #[derive(Default)]
    struct Fills(Vec<(Rect, Color)>);
    impl PaintTarget for Fills {
        fn push_clip(&mut self, _: Rect) {}
        fn pop_clip(&mut self) {}
        fn fill(&mut self, rect: Rect, color: Color) {
            self.0.push((rect, color));
        }
        fn text(&mut self, _: &str, _: Point, _: Face, _: FontMetrics) {}
    }
    let mut target = Fills::default();
    paint_frame(&frame, metrics.0, &mut target);
    assert_eq!(target.0[1], (spec.viewport, spec.default_face.background));
}

#[test]
fn horizontal_scroll_applies_to_wrapped_and_logical_line_starts() {
    let text = StringSnapshot::new("abcdefghi\njklmnopqr", 1);
    let mut s = spec(16.0, 160.0);
    s.hscroll = 8.0;
    s.point = 4;
    let display = layout_window(&text, &s, &[], &FixedMetrics::default()).unwrap();
    assert_eq!(display.rows.iter().map(|r| r.source.clone()).collect::<Vec<_>>(),
        vec![0..3, 3..6, 6..9, 10..13, 13..16, 16..19]);
    for row in &display.rows {
        assert_eq!(row.clusters[0].bounds.x, -8.0);
    }
    // The visible start of the continuation maps to its second scalar, not
    // the horizontally clipped scalar at the logical visual-row origin.
    let row_y = display.rows[1].bounds.y;
    assert_eq!(display.cursor.as_ref().unwrap().bounds.x, 0.0);
    let frame = frame(display);
    assert_eq!(frame.hit_test(Point { x: 1.0, y: row_y + 1.0 }).unwrap().position, 4);
}
