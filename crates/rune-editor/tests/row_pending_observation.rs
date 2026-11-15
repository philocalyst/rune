#![cfg(feature = "display")]
//! Actual owner snapshot observation, without native shaping or normal UI wiring.
use rune_display::{
    TextSnapshot,
    retained_row_handoff::row_preparation_handoff,
    retained_row_transaction::RetainedRowScope,
};
use rune_editor::{Command, RopeDocument, Session};
use rune_text::{PropSetId, Rope, Snapshot};

#[test]
fn actual_point_aba_revokes_unconsumed_request_before_ui_preparation() {
    let document = RopeDocument::new(Snapshot::new(Rope::from("a\u{301}b")), PropSetId(0));
    let mut editor = Session::new(document, 80, 24).unwrap();
    let before = editor.snapshot();
    let selected = before.views.iter().find(|view| view.id == before.selected_window).unwrap();
    let buffer = before.buffers.iter().find(|buffer| buffer.id == selected.view.buffer).unwrap();
    let captured = RetainedRowScope {
        sequence: before.sequence, window: selected.id.as_u64(), buffer: buffer.id.as_u64(),
        text_epoch: buffer.version, source_start_scalar: 0,
        style_revision: 1, viewport_revision: 1, font_bank_generation: 1,
        shaping_policy_revision: 1,
    };
    let (mut preparation, ui) = row_preparation_handoff().unwrap();
    preparation.prepare_snapshot(captured, &buffer.display_snapshot()).unwrap();
    editor.execute(Command::GoTo(1)).unwrap();
    editor.execute(Command::GoTo(0)).unwrap();
    let after = editor.snapshot();
    let source = after.buffers.iter().find(|candidate| candidate.id == buffer.id)
        .unwrap().display_snapshot();
    let text: String = source.chars_from(0).collect();
    let mut fresh = captured;
    fresh.sequence = after.sequence;
    fresh.text_epoch = source.revision();
    assert_ne!(fresh.sequence, captured.sequence);
    assert_eq!(fresh.text_epoch, captured.text_epoch);
    assert!(preparation.observe_owner_state(fresh, &text).unwrap().pending_revoked);
    assert!(ui.next_live_request().unwrap().is_none());
    assert_eq!(editor.point(), 0);
    assert_eq!(text, "a\u{301}b");
    assert_eq!(editor.snapshot().sequence, after.sequence);
    preparation.prepare_snapshot(fresh, &source).unwrap();
    assert_eq!(ui.next_live_request().unwrap().unwrap().scope(), fresh);
}
