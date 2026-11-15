//! Explicit non-IME owner font mode. All cross-thread values are plain DTOs.
use crate::editor_bridge::BridgeError;
use rune_display::{
    FrameDisplay, FrameFontAssignment, FrameFontPlan, MeasuredFontCapabilities, MeasuredFontVariant,
};
use rune_editor::{BufferId, Document, EditorHost, WindowId};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{Arc, Mutex},
};
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedFontRun {
    pub range: Range<usize>,
    pub variant: MeasuredFontVariant,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FontRunProjection {
    pub window_id: u64,
    pub buffer_id: u64,
    pub publication_epoch: u64,
    pub owner_sequence: u64,
    pub frame_id: u64,
    pub visible: Range<usize>,
    pub capabilities: MeasuredFontCapabilities,
    pub runs: Vec<ProjectedFontRun>,
}
pub type FontProjector<H> = fn(
    &H,
    WindowId,
    BufferId,
    usize,
    Range<usize>,
    MeasuredFontCapabilities,
) -> Result<FontRunProjection, String>;
pub(crate) struct Configuration<H: EditorHost> {
    pub capabilities: MeasuredFontCapabilities,
    pub project: FontProjector<H>,
}
pub(crate) type VisibleRanges = BTreeMap<u64, (WindowId, BufferId, usize, Range<usize>)>;
fn error(message: &str) -> BridgeError {
    BridgeError::Editor(rune_editor::EditorError::Backend(message.into()))
}
fn project<H: EditorHost>(
    host: &H,
    c: &Configuration<H>,
    scope: &(WindowId, BufferId, usize, Range<usize>),
    sequence: u64,
    frame_id: u64,
) -> Result<FontRunProjection, String> {
    let (window, buffer, len, visible) = scope;
    if frame_id == 0
        || visible.start > visible.end
        || visible.end > *len
        || visible.end - visible.start > 100000
    {
        return Err("invalid font projection scope budget".into());
    }

    let p = (c.project)(
        host,
        *window,
        *buffer,
        *len,
        visible.clone(),
        c.capabilities,
    )
    .map_err(|e| {
        if e.len() <= 4096 {
            e
        } else {
            "oversized font projection error".into()
        }
    })?;
    if p.window_id != window.as_u64()
        || p.buffer_id != buffer.as_u64()
        || p.owner_sequence != sequence
        || p.frame_id != frame_id
        || p.visible != *visible
        || p.capabilities != c.capabilities
    {
        return Err("font projection scope mismatch".into());
    }
    if visible.start > visible.end
        || visible.end > *len
        || visible.end - visible.start > 100000
        || p.runs.len() > 4096
    {
        return Err("font projection range or run budget exceeded".into());
    }
    let mut end = visible.start;
    for run in &p.runs {
        if run.range.start != end
            || run.range.start >= run.range.end
            || run.range.end > visible.end
            || !c.capabilities.supports_variant(
                c.capabilities.bank_id,
                c.capabilities.metrics,
                run.variant,
            )
        {
            return Err("invalid or unsupported projected font run".into());
        }
        end = run.range.end;
    }
    if end != visible.end {
        return Err("incomplete projected font coverage".into());
    }
    validate_grapheme_projection(host, &p, *len)?;
    Ok(p)
}

// Whole snapshot context is bounded before materialization. Visible substrings
// alone cannot determine RI parity or arbitrary preceding grapheme context.
fn validate_grapheme_projection<H: EditorHost>(host: &H, p: &FontRunProjection, len: usize) -> Result<(), String> {
    if len > 100_000 { return Err("font grapheme snapshot scalar budget exceeded".into()); }
    let snapshot = host.snapshot().map_err(|_| "font grapheme snapshot unavailable")?;
    if snapshot.sequence != p.owner_sequence { return Err("font grapheme sequence mismatch".into()); }
    let buffer = snapshot.buffers.iter().find(|b| b.id.as_u64() == p.buffer_id)
        .ok_or("font grapheme buffer missing")?;
    if buffer.version != p.publication_epoch || buffer.document.len_chars() != len {
        return Err("font grapheme document mismatch".into());
    }
    let mut text = String::new();
    let mut count = 0usize;
    let mut invalid = false;
    buffer.document.visit_chars(0, len, &mut |position, ch| {
        if position != count || count >= len || text.len() + ch.len_utf8() > 1024 * 1024 {
            invalid = true; return false;
        }
        text.push(ch); count += 1; true
    });
    if invalid || count != len { return Err("font grapheme snapshot incomplete or oversized".into()); }
    // Extend endpoint variants only for boundary validation, never for painting.
    // This does not invent projected styles outside the requested visible range.
    let mut runs: Vec<rune_display::GraphemeFontRun> = p.runs.iter().map(|r|
        rune_display::GraphemeFontRun { source: r.range.clone(), variant: r.variant }).collect();
    if let Some(first) = runs.first_mut() { first.source.start = 0; }
    if let Some(last) = runs.last_mut() { last.source.end = len; }
    // Empty visible ranges contain no font transitions to validate.
    if runs.is_empty() { return Ok(()); }
    rune_display::validate_grapheme_font_runs(&text, &runs).map_err(str::to_owned)
}
pub(crate) fn revision<H: EditorHost>(
    host: &H,
    c: &Configuration<H>,
    ranges: &VisibleRanges,
    sequence: u64,
    frame_id: u64,
) -> Vec<Result<FontRunProjection, String>> {
    if ranges.len() > 256 {
        return vec![Err("font window budget exceeded".into())];
    }
    ranges
        .values()
        .map(|scope| project(host, c, scope, sequence, frame_id))
        .collect()
}
pub(crate) fn prepare<H: EditorHost>(
    host: &H,
    c: &Configuration<H>,
    mut frame: Arc<FrameDisplay>,
    ranges: &VisibleRanges,
    sequence: u64,
    expected_frame_id: u64,
) -> Result<Arc<FrameFontPlan>, BridgeError> {
    // Validate structural limits before cloning/mutating any presentation rows.
    FrameFontPlan::new(
        frame.clone(),
        c.capabilities.bank_id,
        c.capabilities.metrics,
        vec![],
    )
    .map_err(error)?;
    if ranges.len() != frame.windows.len() {
        return Err(error("font window scope count mismatch"));
    }
    let mut ids: BTreeMap<(u64, u8), u64> = BTreeMap::new();
    let mut assignments = Vec::new();
    let mut frame_id = None;
    for window in &mut Arc::make_mut(&mut frame).windows {
        let scope = ranges
            .get(&window.window_id)
            .ok_or_else(|| error("missing font window scope"))?;
        let p = project(host, c, scope, sequence, expected_frame_id).map_err(|e| error(&e))?;
        if p.publication_epoch != window.revision || p.buffer_id != window.buffer_id {
            return Err(error("font document epoch mismatch"));
        }
        if frame_id.is_some_and(|id| id != p.frame_id) {
            return Err(error("inconsistent owner font frame identity"));
        }
        frame_id = Some(p.frame_id);
        let mut index = 0;
        for row in &mut window.rows {
            for cluster in &mut Arc::make_mut(row).clusters {
                while index < p.runs.len() && p.runs[index].range.end <= cluster.source.start {
                    index += 1
                }
                let run = p
                    .runs
                    .get(index)
                    .ok_or_else(|| error("cluster outside font projection"))?;
                if cluster.source.start >= cluster.source.end
                    || cluster.source.start < run.range.start
                    || cluster.source.end > run.range.end
                {
                    return Err(error("font transition inside display cluster unsupported"));
                }
                let key = (cluster.face.id, variant_key(run.variant));
                let id = match ids.get(&key) {
                    Some(id) => *id,
                    None => {
                        if ids.len() >= 256 {
                            return Err(error("font assignment budget exceeded"));
                        }
                        let id = (ids.len() + 1) as u64;
                        ids.insert(key, id);
                        assignments.push(FrameFontAssignment {
                            face_id: id,
                            variant: run.variant,
                        });
                        id
                    }
                };
                cluster.face.id = id;
            }
        }
    }
    if host.snapshot().map_err(BridgeError::Editor)?.sequence != sequence {
        return Err(error("owner changed during font projection"));
    }
    FrameFontPlan::new(
        frame,
        c.capabilities.bank_id,
        c.capabilities.metrics,
        assignments,
    )
    .map(Arc::new)
    .map_err(error)
}
fn variant_key(v: MeasuredFontVariant) -> u8 {
    match v {
        MeasuredFontVariant::Normal => 0,
        MeasuredFontVariant::Bold => 1,
        MeasuredFontVariant::Italic => 2,
        MeasuredFontVariant::BoldItalic => 3,
    }
}
struct Mailbox {
    latest: Option<Arc<FrameFontPlan>>,
    receiver_alive: bool,
}
#[derive(Clone)]
pub struct FontPlanPublisher(Arc<Mutex<Mailbox>>);
pub struct FontPlanReceiver(Arc<Mutex<Mailbox>>);
pub fn font_plan_mailbox() -> (FontPlanPublisher, FontPlanReceiver) {
    let state = Arc::new(Mutex::new(Mailbox {
        latest: None,
        receiver_alive: true,
    }));
    (FontPlanPublisher(state.clone()), FontPlanReceiver(state))
}
impl FontPlanPublisher {
    pub(crate) fn publish(&self, plan: Arc<FrameFontPlan>) -> Result<(), &'static str> {
        let mut state = self.0.lock().map_err(|_| "font plan mailbox poisoned")?;
        if !state.receiver_alive {
            return Err("font plan receiver disconnected");
        }
        if state
            .latest
            .as_ref()
            .is_some_and(|old| old.frame().generation >= plan.frame().generation)
        {
            return Err("stale font plan publication");
        }
        state.latest = Some(plan);
        Ok(())
    }
}
impl FontPlanReceiver {
    pub fn latest(&self) -> Result<Option<Arc<FrameFontPlan>>, &'static str> {
        self.0
            .lock()
            .map(|s| s.latest.clone())
            .map_err(|_| "font plan mailbox poisoned")
    }
}
impl Drop for FontPlanReceiver {
    fn drop(&mut self) {
        if let Ok(mut s) = self.0.lock() {
            s.receiver_alive = false;
            s.latest = None;
        }
    }
}

#[cfg(test)]
mod grapheme_projection_tests {
    use super::*;
    use rune_editor::{Session, StringDocument};
    fn fixture(text: &str, visible: Range<usize>, cuts: &[(Range<usize>, MeasuredFontVariant)]) -> (Session<StringDocument>, FontRunProjection) {
        let host = Session::new(StringDocument::from(text), 80, 24).unwrap();
        let s = host.snapshot();
        let b = &s.buffers[0];
        let p = FontRunProjection {
            window_id: s.selected_window.as_u64(), buffer_id: b.id.as_u64(),
            publication_epoch: b.version, owner_sequence: s.sequence, frame_id: 1,
            visible, capabilities: MeasuredFontCapabilities { bank_id:1, metrics:rune_display::FontMetrics {advance:8.0,ascent:13.0,descent:4.0,line_gap:3.0}, normal:true,bold:true,italic:true,bold_italic:true },
            runs: cuts.iter().map(|(range,variant)|ProjectedFontRun {range:range.clone(),variant:*variant}).collect(),
        };
        (host,p)
    }
    #[test]
    fn full_document_context_rejects_visible_ri_split_without_rewriting_projection() {
        let (host,p)=fixture("🇦🇧🇨🇩",1..4,&[(1..3,MeasuredFontVariant::Normal),(3..4,MeasuredFontVariant::Bold)]);
        let before=p.clone();
        assert!(validate_grapheme_projection(&host,&p,4).is_err());
        assert_eq!(p,before);
        let (host,p)=fixture("🇦🇧🇨🇩",1..4,&[(1..2,MeasuredFontVariant::Normal),(2..4,MeasuredFontVariant::Bold)]);
        assert!(validate_grapheme_projection(&host,&p,4).is_ok());
    }
    #[test]
    fn stale_document_and_sequence_and_budget_reject() {
        let (host,mut p)=fixture("ab",0..2,&[(0..2,MeasuredFontVariant::Normal)]);
        p.owner_sequence+=1;
        assert!(validate_grapheme_projection(&host,&p,2).is_err());
        p.owner_sequence-=1;p.publication_epoch+=1;
        assert!(validate_grapheme_projection(&host,&p,2).is_err());
        p.publication_epoch-=1;
        assert!(validate_grapheme_projection(&host,&p,3).is_err());
        assert!(validate_grapheme_projection(&host,&p,100_001).is_err());
    }
    #[test]
    fn actual_project_path_rejects_then_repairs_without_offset_changes() {
        fn projector(host: &Session<StringDocument>, window: WindowId, buffer: BufferId, _: usize, visible: Range<usize>, capabilities: MeasuredFontCapabilities) -> Result<FontRunProjection,String> {
            let s=host.snapshot();
            Ok(FontRunProjection { window_id:window.as_u64(),buffer_id:buffer.as_u64(),publication_epoch:s.buffers[0].version,owner_sequence:s.sequence,frame_id:1,visible,capabilities,
                runs:vec![ProjectedFontRun {range:1..3,variant:MeasuredFontVariant::Normal},ProjectedFontRun {range:3..4,variant:MeasuredFontVariant::Bold}] })
        }
        fn repaired(host: &Session<StringDocument>, window: WindowId, buffer: BufferId, len: usize, visible: Range<usize>, capabilities: MeasuredFontCapabilities) -> Result<FontRunProjection,String> {
            let mut p=projector(host,window,buffer,len,visible,capabilities)?;
            p.runs[0].range.end=2;p.runs[1].range.start=2;Ok(p)
        }
        let (host,p)=fixture("🇦🇧🇨🇩",1..4,&[]);
        let s=host.snapshot();let scope=(s.selected_window,s.buffers[0].id,4,1..4);
        let config=Configuration {capabilities:p.capabilities,project:projector};
        assert!(project(&host,&config,&scope,s.sequence,1).is_err());
        let config=Configuration {capabilities:p.capabilities,project:repaired};
        let result=project(&host,&config,&scope,s.sequence,1).unwrap();
        assert_eq!(result.visible,1..4);assert_eq!(result.runs[0].range,1..2);
        assert_eq!(host.snapshot().sequence,s.sequence);
    }

}
