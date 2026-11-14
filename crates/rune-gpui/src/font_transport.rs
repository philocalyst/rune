//! Explicit non-IME owner font mode. All cross-thread values are plain DTOs.
use crate::editor_bridge::BridgeError;
use rune_display::{
    FrameDisplay, FrameFontAssignment, FrameFontPlan, MeasuredFontCapabilities, MeasuredFontVariant,
};
use rune_editor::{BufferId, EditorHost, WindowId};
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
    Ok(p)
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
