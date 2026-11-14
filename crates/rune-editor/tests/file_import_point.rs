use rune_editor::*;
#[test]
fn import_is_one_edit_preserving_point_epoch_and_normal_other_view_markers() {
 let mut s=Session::new(StringDocument::from("ab"),80,24).unwrap();let b=s.current_buffer();s.execute(Command::GoTo(1)).unwrap();let w=s.selected_window();s.execute(Command::Split(Axis::Vertical)).unwrap();let seq=s.snapshot().sequence;
 s.insert_buffer_preserving_point(b,0,1,"λ🙂").unwrap();assert_eq!(s.point(),1);assert_eq!(s.snapshot().sequence,seq+1);assert_eq!(s.document(b).unwrap().to_text(),"aλ🙂b");assert!(s.snapshot().views.iter().all(|v|v.view.point==1));assert!(s.insert_buffer_preserving_point(b,0,1,"stale").is_err());s.execute(Command::Undo).unwrap();assert_eq!(s.document(b).unwrap().to_text(),"ab");s.execute(Command::SelectWindow(w)).unwrap();s.set_read_only(b,true).unwrap();let read=s.snapshot();assert!(s.insert_buffer_preserving_point(b,s.buffer_snapshot(b).unwrap().version,s.point(),"blocked").is_err());assert_eq!(s.snapshot().sequence,read.sequence);assert_eq!(s.document(b).unwrap().to_text(),"ab");
}
