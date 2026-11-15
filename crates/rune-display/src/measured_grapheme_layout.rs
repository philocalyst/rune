//! Explicit bounded owner layout using producer-supplied contextual widths.
//! Numeric validation cannot prove a producer shaped text. Never promote scalar
//! advance sums or unavailable metrics to this trusted producer contract.
use crate::*;
use std::{ops::Range,sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct MeasuredLayoutScope {pub window_id:u64,pub buffer_id:u64,pub owner_sequence:u64,pub publication_epoch:u64}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum WidthProvenance {
    /// Assertion by a trusted external producer, not a measured-width proof.
    ProducerAssertedContextual { producer_id:u64 },
    /// Explicitly inadmissible for owner geometry.
    DiagnosticScalarAdvanceSum,
    Unavailable,
}
#[derive(Clone,Debug,PartialEq)]
pub struct ContextualClusterWidth {pub source:Range<usize>,pub width:f32}
/// Plain immutable inputs. No native Font/Context or Lisp object is retained.
#[derive(Clone,Debug)]
pub struct MeasuredGraphemeLayoutPlan {
    pub scope:MeasuredLayoutScope,pub capabilities:MeasuredFontCapabilities,
    pub text:Arc<str>,pub provenance:WidthProvenance,pub clusters:Vec<ContextualClusterWidth>,
}
/// Experimental single-logical-line, normal-face path. Not called by the normal
/// renderer. Complete source <=64 scalars/256 bytes; no controls, scrolling,
/// partial viewport or interior exact boundaries. Widths must be externally
/// supplied; this function does not measure or shape text.
pub fn layout_measured_graphemes(snapshot:&dyn TextSnapshot,spec:&WindowSpec,scope:MeasuredLayoutScope,
    capabilities:MeasuredFontCapabilities,face_spans:&[FaceSpan],plan:&MeasuredGraphemeLayoutPlan,exact_boundaries:&[usize])
    ->Result<WindowDisplay,LayoutError>{
    let fail=|s|LayoutError(s);let metrics=capabilities.metrics;
    if !face_spans.is_empty(){return Err(fail("measured face spans unsupported"))}
    if plan.scope!=scope || scope.window_id==0 || scope.buffer_id==0 || scope.window_id!=spec.window_id || scope.buffer_id!=spec.buffer_id || scope.publication_epoch!=snapshot.revision()
        || plan.capabilities!=capabilities || !capabilities.supports_variant(capabilities.bank_id,metrics,MeasuredFontVariant::Normal){return Err(fail("stale measured grapheme scope or bank"))}
    if !matches!(plan.provenance,WidthProvenance::ProducerAssertedContextual{producer_id} if producer_id!=0){return Err(fail("contextual width provenance unavailable"))}
    let len=snapshot.len_chars();
    if len>64 || plan.text.len()>256 || plan.clusters.len()>64 || exact_boundaries.len()>64{return Err(fail("measured grapheme layout budget"))}
    let mut actual=String::new();let mut chars=snapshot.chars_from(0);
    for _ in 0..len {let c=chars.next().ok_or(fail("short measured source"))?;if actual.len()+c.len_utf8()>256{return Err(fail("measured source byte budget"))}actual.push(c)}
    if chars.next().is_some() || actual!=plan.text.as_ref(){return Err(fail("measured source mismatch"))}
    if actual.chars().any(char::is_control){return Err(fail("measured control layout unsupported"))}
    if !spec.viewport.valid() || spec.viewport.width<=0. || !spec.wrap || spec.anchor!=0 || spec.hscroll!=0. || spec.vscroll!=0. || spec.scan_budget<len || spec.point>len || spec.overscan_rows!=0{return Err(fail("measured viewport policy unsupported"))}
    let mut ends=vec![0usize];let mut expected=0usize;let mut byte=0usize;let mut pieces=Vec::new();
    for (g,c) in actual.graphemes(true).zip(plan.clusters.iter()) {
        let end=expected+g.chars().count();
        if c.source!=(expected..end) || !c.width.is_finite() || c.width<=0. || c.width>spec.viewport.width{return Err(fail("invalid or oversized measured cluster"))}
        ends.push(end);pieces.push((byte..byte+g.len(),c));byte+=g.len();expected=end;
    }
    if expected!=len || pieces.len()!=plan.clusters.len() || byte!=actual.len(){return Err(fail("incomplete measured grapheme coverage"))}
    if !ends.contains(&spec.point) || exact_boundaries.iter().any(|p|!ends.contains(p))
        || spec.selection.as_ref().is_some_and(|r|r.start>r.end || !ends.contains(&r.start)||!ends.contains(&r.end)){return Err(fail("exact boundary inside measured grapheme"))}
    let mut rows=Vec::new();let mut clusters=Vec::new();let(mut x,mut y)=(spec.viewport.x,spec.viewport.y);let mut row_start=0;let mut position=0;
    let right=spec.viewport.x+spec.viewport.width;let bottom=spec.viewport.y+spec.viewport.height;
    if !right.is_finite() || !bottom.is_finite() || right<=spec.viewport.x || bottom<=spec.viewport.y{return Err(fail("measured geometry overflow"))}
    for (bytes,c) in pieces {
        if !clusters.is_empty() && x+c.width>right {
            rows.push(Arc::new(DisplayRow{source:row_start..position,bounds:Rect{x:spec.viewport.x,y,width:spec.viewport.width,height:metrics.line_height()},baseline:y+metrics.ascent,continued:row_start!=0,truncated:false,clusters:std::mem::take(&mut clusters)}));
            y+=metrics.line_height();x=spec.viewport.x;row_start=position;
        }
        if !x.is_finite() || !(x+c.width).is_finite() || x+c.width<=x || !y.is_finite() || !(y+metrics.line_height()).is_finite() || y+metrics.line_height()<=y || !(y+metrics.ascent).is_finite() || y+metrics.ascent<=y || y+metrics.line_height()>bottom{return Err(fail("measured layout exceeds viewport"))}
        clusters.push(Cluster{source:c.source.clone(),bounds:Rect{x,y,width:c.width,height:metrics.line_height()},text:Arc::from(&actual[bytes]),face:spec.default_face});x+=c.width;position=c.source.end;
    }
    if !y.is_finite() || !(y+metrics.line_height()).is_finite() || y+metrics.line_height()<=y || !(y+metrics.ascent).is_finite() || y+metrics.ascent<=y || y+metrics.line_height()>bottom{return Err(fail("measured empty row exceeds viewport"))}
    rows.push(Arc::new(DisplayRow{source:row_start..position,bounds:Rect{x:spec.viewport.x,y,width:spec.viewport.width,height:metrics.line_height()},baseline:y+metrics.ascent,continued:row_start!=0,truncated:false,clusters}));
    let mut selection=Vec::new();for row in &rows {for c in &row.clusters {if spec.selection.as_ref().is_some_and(|r|c.source.start<r.end&&c.source.end>r.start){selection.push(c.bounds)}}}
    let cursor=rows.iter().rev().find_map(|row|{let x=row.clusters.iter().find(|c|c.source.start==spec.point).map(|c|c.bounds.x).or_else(||(row.source.end==spec.point).then(||row.clusters.last().map_or(spec.viewport.x,|c|c.bounds.x+c.bounds.width)))?;
        Rect{x,y:row.bounds.y,width:2.,height:metrics.line_height()}.intersection(spec.viewport).map(|bounds|Cursor{position:spec.point,bounds,color:Color(255,205,100,255)})});
    if cursor.is_none(){return Err(fail("measured caret geometry unavailable"))}
    Ok(WindowDisplay{window_id:scope.window_id,buffer_id:scope.buffer_id,revision:scope.publication_epoch,viewport:spec.viewport,background:spec.default_face.background,rows,selection,cursor,next_anchor:len,scanned_chars:len,budget_exhausted:false})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture()->(StringSnapshot,WindowSpec,MeasuredLayoutScope,MeasuredFontCapabilities,MeasuredGraphemeLayoutPlan){
        let text:Arc<str>=Arc::from("a\u{301}bc");let snapshot=StringSnapshot::new(text.clone(),3);
        let mut spec=WindowSpec::new(Rect{x:0.,y:0.,width:20.,height:60.});spec.window_id=7;spec.buffer_id=9;spec.point=2;spec.overscan_rows=0;spec.selection=Some(0..2);
        let scope=MeasuredLayoutScope{window_id:7,buffer_id:9,owner_sequence:12,publication_epoch:3};
        let caps=MeasuredFontCapabilities{bank_id:5,metrics:FixedMetrics::default().0,normal:true,bold:false,italic:false,bold_italic:false};
        // Authenticated fixture numbers, NOT a native shaping measurement.
        let plan=MeasuredGraphemeLayoutPlan{scope,capabilities:caps,text,provenance:WidthProvenance::ProducerAssertedContextual{producer_id:99},clusters:vec![ContextualClusterWidth{source:0..2,width:10.},ContextualClusterWidth{source:2..3,width:12.},ContextualClusterWidth{source:3..4,width:8.}]};(snapshot,spec,scope,caps,plan)
    }
    #[test]fn fixture_widths_drive_owner_rows_hits_and_exact_offsets(){
        let(s,spec,scope,caps,p)=fixture();let w=layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[0,2,4]).unwrap();
        assert_eq!(w.rows.len(),2);assert_eq!(w.rows[0].source,0..2);assert_eq!(w.rows[1].source,2..4);
        assert_eq!(w.rows[0].clusters[0].text.as_ref(),"a\u{301}");assert_eq!(w.rows[0].clusters[0].bounds.width,10.);
        assert_eq!(w.rows[1].clusters[0].bounds.width,12.);assert_eq!(w.rows[1].clusters[1].bounds.x,12.);
        assert_eq!(w.selection[0].width,10.);assert_eq!(w.cursor.as_ref().unwrap().position,2);
        let f=FrameDisplay{protocol_version:DISPLAY_PROTOCOL_VERSION,generation:1,bounds:spec.viewport,background:spec.default_face.background,windows:vec![w]};
        assert_eq!(f.hit_test(Point{x:2.,y:1.}).unwrap().position,0);assert_eq!(f.hit_test(Point{x:8.,y:1.}).unwrap().position,2);
    }
    #[test]fn exact_interior_boundaries_and_styles_are_not_snapped(){let(s,mut spec,scope,caps,p)=fixture();assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[FaceSpan{range:0..1,face:spec.default_face}],&p,&[]).is_err());assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[1]).is_err());spec.point=1;assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[]).is_err());spec.point=2;spec.selection=Some(0..1);assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[]).is_err());}
    #[test]fn reject_stale_missing_and_artificial_metrics(){let(s,spec,scope,caps,mut p)=fixture();for provenance in[WidthProvenance::Unavailable,WidthProvenance::DiagnosticScalarAdvanceSum,WidthProvenance::ProducerAssertedContextual{producer_id:0}]{p.provenance=provenance;assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[]).is_err())}p.provenance=WidthProvenance::ProducerAssertedContextual{producer_id:1};p.scope.owner_sequence+=1;assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[]).is_err());p.scope=scope;p.capabilities.bank_id+=1;assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[]).is_err());p.capabilities=caps;p.text=Arc::from("xxxx");assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&p,&[]).is_err());}
    #[test]fn invalid_widths_coverage_geometry_and_controls_fail_closed(){let(s,spec,scope,caps,p)=fixture();for width in[f32::NAN,f32::INFINITY,-1.,0.,21.]{let mut q=p.clone();q.clusters[0].width=width;assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&q,&[]).is_err())}let mut q=p.clone();q.clusters[0].source=0..1;assert!(layout_measured_graphemes(&s,&spec,scope,caps,&[],&q,&[]).is_err());let mut lost=spec.clone();lost.viewport.x=1e20;lost.viewport.width=1e20;assert!(layout_measured_graphemes(&s,&lost,scope,caps,&[],&p,&[]).is_err());let mut lost=spec.clone();lost.viewport.y=1e20;lost.viewport.height=1e20;assert!(layout_measured_graphemes(&s,&lost,scope,caps,&[],&p,&[]).is_err());let mut caret=spec.clone();caret.viewport.x=100000000.;caret.viewport.width=1000.;assert!(layout_measured_graphemes(&s,&caret,scope,caps,&[],&p,&[]).is_err());let mut baseline=spec.clone();baseline.viewport.y=268435456.;baseline.viewport.height=1024.;assert!(layout_measured_graphemes(&s,&baseline,scope,caps,&[],&p,&[]).is_err());let mut huge=spec.clone();huge.viewport.x=f32::MAX/2.;assert!(layout_measured_graphemes(&s,&huge,scope,caps,&[],&p,&[]).is_err());let mut small=spec.clone();small.viewport.height=20.;assert!(layout_measured_graphemes(&s,&small,scope,caps,&[],&p,&[]).is_err());let mut q=p.clone();q.text=Arc::from("a\nbc");let newline=StringSnapshot::new(q.text.clone(),3);assert!(layout_measured_graphemes(&newline,&spec,scope,caps,&[],&q,&[]).is_err());}
}
