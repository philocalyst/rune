#![cfg(feature="display")]
use rune_editor::*;
use rune_display::*;
use rune_text::{Rope,Snapshot,PropSetId};
use std::sync::Arc;
#[test]
fn actual_owner_snapshot_admission_rejects_edit_and_point_aba_without_moving_input(){
    let doc=RopeDocument::new(Snapshot::new(Rope::from("a\u{301}b")),PropSetId(0));
    let mut owner=Session::new(doc,80,24).unwrap();let before=owner.snapshot();let buffer=&before.buffers[0];
    let published=buffer.display_snapshot();let scope=MeasuredLayoutScope{window_id:before.selected_window.as_u64(),buffer_id:buffer.id.as_u64(),owner_sequence:before.sequence,publication_epoch:buffer.version};
    let caps=MeasuredFontCapabilities{bank_id:7,metrics:FixedMetrics::default().0,normal:true,bold:false,italic:false,bold_italic:false};
    // Fixture widths are trusted test input, not an actual native measurement.
    let plan=MeasuredGraphemeLayoutPlan{scope,capabilities:caps,text:Arc::from("a\u{301}b"),provenance:WidthProvenance::ProducerAssertedContextual{producer_id:1},clusters:vec![ContextualClusterWidth{source:0..2,width:12.},ContextualClusterWidth{source:2..3,width:9.}]};
    let mut spec=WindowSpec::new(Rect{x:0.,y:0.,width:16.,height:60.});spec.window_id=scope.window_id;spec.buffer_id=scope.buffer_id;spec.overscan_rows=0;
    let output=layout_measured_graphemes(&published,&spec,scope,caps,&[],&plan,&[]).unwrap();assert_eq!(output.rows[0].source,0..2);assert_eq!(owner.point(),0);
    owner.execute(Command::GoTo(1)).unwrap();owner.execute(Command::GoTo(0)).unwrap();let now=owner.snapshot();let mut fresh=scope;fresh.owner_sequence=now.sequence;
    assert!(layout_measured_graphemes(&now.buffers[0].display_snapshot(),&spec,fresh,caps,&[],&plan,&[]).is_err());assert_eq!(owner.point(),0);
    owner.execute(Command::Insert("x".into())).unwrap();let after=owner.snapshot();fresh.owner_sequence=after.sequence;fresh.publication_epoch=after.buffers[0].version;
    assert!(layout_measured_graphemes(&after.buffers[0].display_snapshot(),&spec,fresh,caps,&[],&plan,&[]).is_err());assert_eq!(owner.document(scope_buffer(&after)).unwrap().to_text(),"xa\u{301}b");
}
fn scope_buffer(s:&SessionSnapshot<RopeDocument>)->BufferId{s.buffers[0].id}
