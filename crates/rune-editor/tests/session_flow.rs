use rune_editor::*;
#[test]
fn unicode_edit_undo_windows_and_buffer_liveness() {
    let mut session = Session::new(StringDocument::from("aλ\ntext"), 80, 24).unwrap();
    let original = session.current_buffer();
    session.execute(Command::GoTo(2)).unwrap();
    session.execute(Command::Insert("🙂".into())).unwrap();
    assert_eq!(session.document(original).unwrap().to_text(), "aλ🙂\ntext");
    session.execute(Command::Split(Axis::Vertical)).unwrap();
    session.execute(Command::NextWindow).unwrap();
    session.execute(Command::Undo).unwrap();
    assert_eq!(session.document(original).unwrap().to_text(), "aλ\ntext");
    session.execute(Command::Redo).unwrap();
    let other = session.create_buffer("other", StringDocument::from("safe")).unwrap();
    session.execute(Command::KillBuffer { buffer: original, force: true }).unwrap();
    assert!(!session.buffer_live(original));
    assert_eq!(session.current_buffer(), other);
    assert!(session.snapshot().views.iter().all(|view| view.view.buffer == other));
    session.check_invariants().unwrap();
}
#[test]
fn prefix_resize_and_failed_command_are_transactional() {
    let mut session = Session::new(StringDocument::empty(), 12, 4).unwrap();
    assert!(matches!(session.dispatch(Key::Control('x')).unwrap(), Dispatch::Pending(_)));
    session.dispatch(Key::Char('3')).unwrap();
    let before = session.window_tree().clone();
    assert!(session.execute(Command::Resize { width: 1, height: 1 }).is_err());
    assert_eq!(*session.window_tree(), before);
    assert!(session.execute(Command::DeleteBackward(1)).is_err());
    assert_eq!(session.point(), 0);
    session.check_invariants().unwrap();
}
#[cfg(feature = "rope")]
#[test]
fn rope_properties_and_markers_survive_session_undo() {
    use rune_text::{CharPos, PropSetId, Rope, Snapshot};
    let (snapshot, _) = Snapshot::new(Rope::from("ab"))
        .edit(CharPos(0)..CharPos(1), "λ", PropSetId(7))
        .unwrap();
    let mut session = Session::new(RopeDocument::new(snapshot, PropSetId(9)), 80, 24).unwrap();
    let id = session.current_buffer();
    let original = session.document(id).unwrap().clone();
    session.execute(Command::GoTo(1)).unwrap();
    session.execute(Command::Insert("🙂".into())).unwrap();
    let edited = session.document(id).unwrap();
    assert_eq!(edited.snapshot().properties().at(CharPos(0)), Some(PropSetId(7)));
    assert_eq!(edited.snapshot().properties().at(CharPos(1)), Some(PropSetId(9)));
    assert_eq!(original.to_text(), "λb");
    session.execute(Command::Undo).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "λb");
    assert_eq!(
        session.document(id).unwrap().snapshot().properties().at(CharPos(0)),
        Some(PropSetId(7))
    );
}

#[test]
fn user_save_undo_redo_and_branching_history() {
    let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
    let id = session.current_buffer();
    session.execute(Command::Insert("first".into())).unwrap();
    session.mark_saved(id).unwrap();
    session.execute(Command::Insert(" second".into())).unwrap();
    assert!(session.modified(id).unwrap());
    session.execute(Command::Undo).unwrap();
    assert!(!session.modified(id).unwrap());
    session.execute(Command::Redo).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "first second");
    session.execute(Command::Undo).unwrap();
    session.execute(Command::Insert(" branch".into())).unwrap();
    assert_eq!(session.execute(Command::Redo), Err(EditorError::NoRedo));
    assert_eq!(session.document(id).unwrap().to_text(), "first branch");
}

#[test]
fn user_window_excursion_restores_layout_after_buffer_death() {
    let mut session = Session::new(StringDocument::from("original"), 80, 24).unwrap();
    let original = session.current_buffer();
    session.execute(Command::Split(Axis::Horizontal)).unwrap();
    session.execute(Command::NextWindow).unwrap();
    session.execute(Command::GoTo(4)).unwrap();
    let configuration = session.window_configuration();
    let fallback = session.create_buffer("replacement", StringDocument::from("x")).unwrap();
    session.execute(Command::DeleteOtherWindows).unwrap();
    session.execute(Command::KillBuffer { buffer: original, force: true }).unwrap();
    session.restore_window_configuration(&configuration).unwrap();
    assert_eq!(session.snapshot().views.len(), 2);
    assert!(
        session
            .snapshot()
            .views
            .iter()
            .all(|view| view.view.buffer == fallback && view.view.point <= 1)
    );
    session.check_invariants().unwrap();
    let foreign = Session::new(StringDocument::empty(), 10, 10).unwrap();
    assert_eq!(
        session.restore_window_configuration(&foreign.window_configuration()),
        Err(EditorError::ForeignConfiguration)
    );
}

#[test]
fn user_records_unicode_macro_and_continues_after_read_only_error() {
    let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
    let id = session.current_buffer();
    session.start_macro().unwrap();
    session.dispatch(Key::Char('λ')).unwrap();
    session.dispatch(Key::Char('🙂')).unwrap();
    assert_eq!(session.finish_macro().unwrap(), 2);
    session.replay_macro(2).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "λ🙂λ🙂λ🙂");
    session.set_read_only(id, true).unwrap();
    assert_eq!(session.execute(Command::Insert("bad".into())), Err(EditorError::ReadOnly));
    assert_eq!(session.point(), 6);
    session.set_read_only(id, false).unwrap();
    session.execute(Command::Insert("!".into())).unwrap();
    session.check_invariants().unwrap();
}

#[cfg(feature = "display")]
#[test]
fn renderer_reads_immutable_property_snapshot_after_later_edit() {
    use rune_display::TextSnapshot;
    use rune_text::{CharPos, PropSetId, Rope, Snapshot};
    let mut session =
        Session::new(RopeDocument::new(Snapshot::new(Rope::from("aλ\n🙂")), PropSetId(3)), 80, 24)
            .unwrap();
    let id = session.current_buffer();
    let published = session.snapshot().buffers.iter().find(|buffer| buffer.id == id).unwrap().display_snapshot();
    session.execute(Command::Insert("中".into())).unwrap();
    assert_eq!(TextSnapshot::chars_from(&published, 1).collect::<String>(), "λ\n🙂");
    assert_eq!(TextSnapshot::next_line_start(&published, 0), Some(3));
    assert_eq!(TextSnapshot::chars_from(&published, usize::MAX).count(), 0);
    assert_eq!(
        session.document(id).unwrap().snapshot().properties().at(CharPos(0)),
        Some(PropSetId(3))
    );
    session.execute(Command::Undo).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), published.to_text());
}

#[test]
fn user_vertical_motion_clamps_unicode_column_and_rejects_overrun_atomically() {
    let mut session = Session::new(StringDocument::from("λ🙂abc\nx\n界界界\n"), 80, 24).unwrap();
    session.execute(Command::GoTo(4)).unwrap();
    session.dispatch(Key::Down).unwrap();
    assert_eq!(session.point(), 7); // Short line clamps before newline.
    session.dispatch(Key::Down).unwrap();
    assert_eq!(session.point(), 9); // Uses current scalar column, not bytes.
    session.execute(Command::MoveLines(1)).unwrap();
    assert_eq!(session.point(), 12); // Final empty line.
    assert_eq!(session.execute(Command::MoveLines(1)), Err(EditorError::InvalidPosition));
    assert_eq!(session.point(), 12);
    session.execute(Command::MoveLines(-3)).unwrap();
    assert_eq!(session.point(), 0);
    session.check_invariants().unwrap();
}

#[test]
fn formatter_replacement_updates_other_view_anchors_and_undo() {
    let mut session = Session::new(StringDocument::from("aλbcdef"), 80, 24).unwrap();
    let id = session.current_buffer();
    session.execute(Command::GoTo(6)).unwrap();
    session.execute(Command::Split(Axis::Vertical)).unwrap();
    let original_window = session.selected_window();
    session.execute(Command::NextWindow).unwrap();
    session.execute(Command::GoTo(1)).unwrap();
    session
        .execute(Command::ReplaceRange { range: 1..4, text: "🙂".into() })
        .unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "a🙂def");
    assert_eq!(session.window_tree().view(original_window).unwrap().point, 4);
    assert_eq!(session.point(), 2);
    session.execute(Command::Undo).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "aλbcdef");
    assert_eq!(session.window_tree().view(original_window).unwrap().point, 6);
    session.execute(Command::Redo).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "a🙂def");
    session.check_invariants().unwrap();
}

#[test]
fn replacement_invalid_ranges_and_read_only_leave_text_and_history_unchanged() {
    let mut session = Session::new(StringDocument::from("λ🙂"), 80, 24).unwrap();
    let id = session.current_buffer();
    for range in [3..3, 2..1, 0..usize::MAX] {
        assert!(session.execute(Command::ReplaceRange { range, text: String::new() }).is_err());
    }
    session.set_read_only(id, true).unwrap();
    assert_eq!(
        session.execute(Command::ReplaceRange { range: 0..1, text: "x".into() }),
        Err(EditorError::ReadOnly)
    );
    assert_eq!(session.document(id).unwrap().to_text(), "λ🙂");
    assert!(!session.modified(id).unwrap());
    session.set_read_only(id, false).unwrap();
    assert_eq!(session.execute(Command::Undo), Err(EditorError::NoUndo));
}

#[test]
fn literal_search_round_trip_unicode_and_missing_match_preserves_point() {
    let mut session = Session::new(StringDocument::from("aλ🙂 b λ🙂"), 80, 24).unwrap();
    let id = session.current_buffer();
    session.execute(Command::FindForward("λ🙂".into())).unwrap();
    assert_eq!(session.point(), 3);
    session.execute(Command::FindForward("λ🙂".into())).unwrap();
    assert_eq!(session.point(), 8);
    session.execute(Command::FindBackward("λ🙂".into())).unwrap();
    assert_eq!(session.point(), 6);
    session.execute(Command::FindBackward("λ🙂".into())).unwrap();
    assert_eq!(session.point(), 1);
    assert_eq!(
        session.execute(Command::FindBackward("missing".into())),
        Err(EditorError::SearchFailed)
    );
    assert_eq!(session.point(), 1);
    session.execute(Command::FindForward(String::new())).unwrap();
    assert_eq!(session.point(), 1);
    assert!(!session.modified(id).unwrap());
    assert_eq!(session.execute(Command::Undo), Err(EditorError::NoUndo));
}

#[test]
fn queued_user_events_recover_after_failed_command_and_prefix() {
    let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
    let id = session.current_buffer();
    session.enqueue(InputEvent::Key(Key::Char('λ')));
    session.enqueue(InputEvent::Command(Command::GoTo(usize::MAX)));
    session.enqueue(InputEvent::Key(Key::Control('x')));
    session.enqueue(InputEvent::Key(Key::Char('3')));
    session.enqueue(InputEvent::Command(Command::NextWindow));
    session.enqueue(InputEvent::Key(Key::Char('🙂')));
    let mut errors = 0;
    while let Some(outcome) = session.step() { if outcome.is_err() { errors += 1; } }
    assert_eq!(errors, 1);
    assert_eq!(session.document(id).unwrap().to_text(), "λ🙂");
    assert_eq!(session.snapshot().views.len(), 2);
    assert!(session.step().is_none());
    session.check_invariants().unwrap();
}

#[test]
fn user_named_buffer_switch_restores_caret_and_independent_undo() {
    let mut session = Session::new(StringDocument::from("original"), 80, 24).unwrap();
    let original = session.current_buffer();
    session.execute(Command::GoTo(3)).unwrap();
    session.execute(Command::SwitchToBuffer { name: "notes".into(), create: true }).unwrap();
    let notes = session.current_buffer();
    session.execute(Command::Insert("λ🙂".into())).unwrap();
    session.execute(Command::SwitchToBuffer { name: "*scratch*".into(), create: false }).unwrap();
    assert_eq!(session.current_buffer(), original);
    assert_eq!(session.point(), 3);
    assert_eq!(session.execute(Command::Undo), Err(EditorError::NoUndo));
    session.execute(Command::SwitchToBuffer { name: "notes".into(), create: true }).unwrap();
    assert_eq!(session.current_buffer(), notes);
    assert_eq!(session.point(), 2);
    session.execute(Command::Undo).unwrap();
    assert_eq!(session.document(notes).unwrap().to_text(), "");
    assert_eq!(session.document(original).unwrap().to_text(), "original");
    assert_eq!(session.buffer_list().len(), 2);
    assert!(session.execute(Command::SwitchToBuffer { name: "missing".into(), create: false }).is_err());
    assert_eq!(session.current_buffer(), notes);
    session.check_invariants().unwrap();
}

#[cfg(feature = "display")]
#[test]
fn branching_undo_publication_never_reuses_revision_for_different_text() {
    use rune_display::TextSnapshot;
    let mut session = Session::new(RopeDocument::empty(), 80, 24).unwrap();
    let id = session.current_buffer();
    session.execute(Command::Insert("a".into())).unwrap();
    let first = session.snapshot().buffers.iter().find(|buffer| buffer.id == id).unwrap().display_snapshot();
    session.execute(Command::Undo).unwrap();
    session.execute(Command::Insert("b".into())).unwrap();
    let branch = session.snapshot().buffers.iter().find(|buffer| buffer.id == id).unwrap().display_snapshot();
    assert_ne!(TextSnapshot::revision(&first), TextSnapshot::revision(&branch));
    assert_eq!(TextSnapshot::chars_from(&first, 0).collect::<String>(), "a");
    assert_eq!(TextSnapshot::chars_from(&branch, 0).collect::<String>(), "b");
}

#[test]
fn publication_epoch_increases_on_undo_redo_without_breaking_savepoints() {
    let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
    let id = session.current_buffer();
    let epoch = |session: &Session<StringDocument>| session.snapshot().buffers.iter().find(|buffer| buffer.id == id).unwrap().version;
    session.execute(Command::Insert("a".into())).unwrap();
    session.mark_saved(id).unwrap();
    let a = epoch(&session);
    session.execute(Command::Insert("b".into())).unwrap();
    let b = epoch(&session);
    session.execute(Command::Undo).unwrap();
    let undo = epoch(&session);
    assert!(!session.modified(id).unwrap());
    session.execute(Command::Redo).unwrap();
    let redo = epoch(&session);
    assert!(session.modified(id).unwrap());
    assert!(a < b && b < undo && undo < redo);
}

#[test]
fn committed_ime_text_is_one_transaction_not_control_key_translation() {
    let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
    let id = session.current_buffer();
    session.enqueue(InputEvent::Key(Key::Control('x')));
    session.enqueue(InputEvent::Text("λ🙂e\u{301}\n".into()));
    assert!(matches!(session.step().unwrap().unwrap(), Dispatch::Pending(_)));
    assert!(matches!(session.step().unwrap().unwrap(), Dispatch::Executed(_)));
    assert_eq!(session.document(id).unwrap().to_text(), "λ🙂e\u{301}\n");
    assert_eq!(session.point(), 5); // Scalars, not bytes or grapheme clusters.
    session.execute(Command::Undo).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "");
    assert_eq!(session.execute(Command::Undo), Err(EditorError::NoUndo));
    session.execute(Command::Redo).unwrap();
    assert_eq!(session.document(id).unwrap().to_text(), "λ🙂e\u{301}\n");
    session.check_invariants().unwrap();
}

#[test]
fn late_native_ime_commit_cannot_silently_target_another_window() {
    let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
    let origin = session.selected_window();
    let buffer = session.current_buffer();
    session.execute(Command::Split(Axis::Vertical)).unwrap();
    session.execute(Command::NextWindow).unwrap();
    session.enqueue(InputEvent::TextForWindow { window: origin, buffer, text: "late".into() });
    assert_eq!(session.step().unwrap(), Err(EditorError::StaleInput));
    assert_eq!(session.document(buffer).unwrap().to_text(), "");
    session.execute(Command::SelectWindow(origin)).unwrap();
    session.enqueue(InputEvent::TextForWindow { window: origin, buffer, text: "λ".into() });
    assert!(matches!(session.step().unwrap().unwrap(), Dispatch::Executed(_)));
    assert_eq!(session.document(buffer).unwrap().to_text(), "λ");
    session.check_invariants().unwrap();
}

#[test]
fn native_commit_rejects_same_window_after_buffer_switch() {
    let mut session = Session::new(StringDocument::empty(), 80, 24).unwrap();
    let window = session.selected_window();
    let buffer = session.current_buffer();
    session.execute(Command::SwitchToBuffer { name: "other".into(), create: true }).unwrap();
    let other = session.current_buffer();
    assert_eq!(session.commit_text_for_buffer(window, buffer, "late".into()), Err(EditorError::StaleInput));
    assert_eq!(session.document(buffer).unwrap().to_text(), "");
    assert_eq!(session.document(other).unwrap().to_text(), "");
    session.check_invariants().unwrap();
}

#[cfg(feature="rope")]
#[test]
fn marker_history_recaptures_live_ids_each_cycle() {
    let mut s=Session::new(RopeDocument::empty(),80,24).unwrap();
    let b=s.current_buffer();
    s.execute(Command::Insert("λ🙂x".into())).unwrap();
    let removed=s.register_marker(b,1,Bias::Left).unwrap();
    s.execute(Command::Undo).unwrap();s.execute(Command::Redo).unwrap();
    let fresh=s.register_marker(b,2,Bias::Left).unwrap();s.remove_marker(b,removed).unwrap();
    for _ in 0..3 {
        s.execute(Command::Undo).unwrap();assert_eq!(s.marker_position(b,fresh).unwrap(),Some(0));
        s.execute(Command::Redo).unwrap();assert_eq!(s.marker_position(b,fresh).unwrap(),Some(2));
        assert_eq!(s.marker_position(b,removed).unwrap(),None);
    }
}
#[cfg(feature="rope")]
#[test]
fn empty_sparse_records_keep_delete_then_insert_marker_order() {
    let d=RopeDocument::new(rune_text::Snapshot::new(rune_text::Rope::from("abc")),rune_text::PropSetId(0));
    let mut s=Session::new(d,80,24).unwrap();let b=s.current_buffer();
    s.register_marker(b,0,Bias::Left).unwrap();s.register_marker(b,3,Bias::Right).unwrap();
    s.execute(Command::ReplaceRange {range:0..3,text:"X".into()}).unwrap();
    let fresh=s.register_marker(b,1,Bias::Left).unwrap();s.execute(Command::Undo).unwrap();
    assert_eq!(s.marker_position(b,fresh).unwrap(),Some(0));
}
