use rune_editor::*;
fn view(s:&Session<StringDocument>,id:WindowId)->(usize,usize) {
    let v=s.snapshot().views.into_iter().find(|v|v.id==id).unwrap().view;(v.point,v.start)
}
fn fixture()->(Session<StringDocument>,BufferId,WindowId,WindowId) {
    let mut s=Session::new(StringDocument::from("abcdef"),80,24).unwrap();
    let b=s.current_buffer();let origin=s.selected_window();s.execute(Command::GoTo(2)).unwrap();s.execute(Command::Split(Axis::Vertical)).unwrap();let other=s.snapshot().views.into_iter().find(|v|v.id!=origin).unwrap().id;s.set_window_start(other,2).unwrap();(s,b,origin,other)
}
#[test]
fn forced_forward_views_and_dynamic_redo_do_not_force_fresh_windows() {
    let(mut s,b,origin,other)=fixture();let epoch=s.buffer_snapshot(b).unwrap().version;
    s.insert_buffer_before_markers(b,epoch,2,"λ🙂").unwrap();assert_eq!(view(&s,other),(4,4));
    s.execute(Command::Undo).unwrap();assert_eq!(view(&s,other),(2,2));
    s.execute(Command::Split(Axis::Horizontal)).unwrap();let fresh=s.snapshot().views.into_iter().find(|v|v.id!=origin&&v.id!=other).unwrap().id;s.set_window_start(fresh,2).unwrap();
    s.execute(Command::Redo).unwrap();assert_eq!(view(&s,other),(4,4));assert_eq!(view(&s,fresh),(2,2));
    s.execute(Command::Undo).unwrap();s.execute(Command::SelectWindow(fresh)).unwrap();s.execute(Command::DeleteWindow).unwrap();s.execute(Command::SelectWindow(origin)).unwrap();s.execute(Command::Redo).unwrap();assert_eq!(view(&s,other),(4,4));assert!(!s.snapshot().views.iter().any(|v|v.id==fresh));s.check_invariants().unwrap();
}
#[test]
fn sparse_view_corrections_follow_current_collapsed_boundary_eligibility() {
    let(mut s,b,origin,other)=fixture();s.insert_buffer_before_markers(b,0,2,"λ🙂").unwrap();s.execute(Command::Undo).unwrap();
    s.execute(Command::SelectWindow(other)).unwrap();s.execute(Command::GoTo(0)).unwrap();s.set_window_start(other,0).unwrap();s.execute(Command::SelectWindow(origin)).unwrap();s.execute(Command::Redo).unwrap();assert_eq!(view(&s,other),(0,0));
    let(mut s,b,origin,other)=fixture();s.insert_buffer_before_markers(b,0,2,"λ🙂").unwrap();s.execute(Command::Undo).unwrap();
    s.execute(Command::SelectWindow(other)).unwrap();s.execute(Command::GoTo(0)).unwrap();s.set_window_start(other,0).unwrap();s.execute(Command::GoTo(2)).unwrap();s.set_window_start(other,2).unwrap();s.execute(Command::SelectWindow(origin)).unwrap();s.execute(Command::Redo).unwrap();assert_eq!(view(&s,other),(4,4));
}
#[test]
fn failed_before_markers_edit_is_atomic_and_epoch_checked() {
    let(mut s,b,_,other)=fixture();let before=s.snapshot();assert!(s.insert_buffer_before_markers(b,1,2,"x").is_err());assert_eq!(s.snapshot().sequence,before.sequence);assert_eq!(view(&s,other),(2,2));assert_eq!(s.document(b).unwrap().to_text(),"abcdef");assert!(s.insert_buffer_before_markers(b,0,99,"x").is_err());assert_eq!(s.document(b).unwrap().to_text(),"abcdef");
}
#[test]
fn empty_sparse_view_records_still_use_ordered_delete_then_insert() {
    let mut s=Session::new(StringDocument::from("abcdef"),80,24).unwrap();let b=s.current_buffer();let origin=s.selected_window();
    // Original deletion captures Some(empty): only zero-offset view fields.
    s.replace_buffer(b,0,0..3,"X").unwrap();
    s.execute(Command::Split(Axis::Vertical)).unwrap();let fresh=s.snapshot().views.into_iter().find(|v|v.id!=origin).unwrap().id;
    s.set_window_start(fresh,1).unwrap();assert_eq!(view(&s,fresh),(1,1));
    s.execute(Command::Undo).unwrap();
    // Fresh inserted-end anchor collapses during deletion and stays LEFT when
    // original text is reinserted. Aggregate old-end mapping would yield 3.
    assert_eq!(view(&s,fresh),(0,0));assert_eq!(s.document(b).unwrap().to_text(),"abcdef");
}
