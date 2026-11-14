use rune_editor::*;
#[test]
fn scroll_is_scoped_clamped_and_never_moves_point_or_text() {
 let mut s=Session::new(StringDocument::from("λ\n🙂\nthird\n"),80,24).unwrap();let b=s.current_buffer();let w=s.selected_window();s.execute(Command::GoTo(1)).unwrap();s.set_read_only(b,true).unwrap();let seq=s.snapshot().sequence;
 assert!(s.scroll_window(w,b,seq,1).unwrap());let after=s.snapshot();assert_eq!(after.views[0].view.start,2);assert_eq!(s.point(),1);assert_eq!(s.document(b).unwrap().to_text(),"λ\n🙂\nthird\n");assert!(s.scroll_window(w,b,seq,1).is_err());
 assert!(s.scroll_window(w,b,after.sequence,i64::MAX).unwrap());assert_eq!(s.snapshot().views[0].view.start,10);let seq=s.snapshot().sequence;assert!(!s.scroll_window(w,b,seq,1).unwrap());assert_eq!(s.snapshot().sequence,seq);
 assert!(s.scroll_window(w,b,seq,i64::MIN).unwrap());assert_eq!(s.snapshot().views[0].view.start,0);assert_eq!(s.point(),1);
}
#[test]
fn scope_aba_and_wrong_buffer_are_rejected_even_zero_delta() {
 let mut s=Session::new(StringDocument::from("a\nb\n"),80,24).unwrap();let b=s.current_buffer();let w=s.selected_window();let seq=s.snapshot().sequence;let other=s.create_buffer("other",StringDocument::empty()).unwrap();assert!(s.scroll_window(w,other,seq,1).is_err());s.execute(Command::Split(Axis::Vertical)).unwrap();s.execute(Command::NextWindow).unwrap();s.execute(Command::SelectWindow(w)).unwrap();assert!(s.scroll_window(w,b,seq,0).is_err());assert_eq!(s.snapshot().views.iter().find(|v|v.id==w).unwrap().view.start,0);
}
#[cfg(feature="rope")]
#[test]
fn rope_scrolling_uses_line_index_for_long_unicode_org() {
 use rune_text::{Rope,Snapshot,PropSetId};let text="* λ heading\nbody🙂\n".repeat(5000);let doc=RopeDocument::new(Snapshot::new(Rope::from(text.as_str())),PropSetId(0));let mut s=Session::new(doc,80,24).unwrap();let b=s.current_buffer();let w=s.selected_window();assert!(s.scroll_window(w,b,s.snapshot().sequence,9998).unwrap());assert_eq!(s.snapshot().views[0].view.start,"* λ heading\nbody🙂\n".chars().count()*4999);assert_eq!(s.point(),0);
}
