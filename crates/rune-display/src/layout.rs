use crate::*;
use std::{ops::Range, sync::Arc};

#[derive(Debug, Clone)]
pub struct WindowSpec {
    pub window_id: u64,
    pub buffer_id: u64,
    pub viewport: Rect,
    pub anchor: usize,
    pub point: usize,
    pub selection: Option<Range<usize>>,
    pub vscroll: f32,
    pub hscroll: f32,
    pub wrap: bool,
    pub tab_width: u8,
    pub overscan_rows: usize,
    pub scan_budget: usize,
    pub default_face: Face,
}
impl WindowSpec {
    pub fn new(viewport: Rect) -> Self {
        Self {
            window_id: 1,
            buffer_id: 1,
            viewport,
            anchor: 0,
            point: 0,
            selection: None,
            vscroll: 0.0,
            hscroll: 0.0,
            wrap: true,
            tab_width: 8,
            overscan_rows: 2,
            scan_budget: 100_000,
            default_face: Face::default(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutError(pub &'static str);
impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for LayoutError {}

/// Pure bounded layout. ANCHOR is an already selected visual-row start; deciding
/// Emacs window-start/scroll policy belongs to prepare/editor, not the painter.
pub fn layout_window(
    snapshot: &dyn TextSnapshot,
    spec: &WindowSpec,
    spans: &[FaceSpan],
    metrics: &dyn Metrics,
) -> Result<WindowDisplay, LayoutError> {
    let font = metrics.font_metrics();
    if !spec.viewport.valid()
        || !font.valid()
        || !spec.vscroll.is_finite()
        || !spec.hscroll.is_finite()
        || spec.vscroll < 0.0
        || spec.vscroll >= font.line_height()
        || spec.hscroll < 0.0
        || spec.tab_width == 0
        || spec.overscan_rows > 64
        || spec.scan_budget == 0
    {
        return Err(LayoutError("invalid viewport, metrics or layout budget"));
    }
    let len = snapshot.len_chars();
    if spec.anchor > len
        || spec.point > len
        || spec.selection.as_ref().is_some_and(|r| r.start > r.end || r.end > len)
    {
        return Err(LayoutError("invalid character position"));
    }
    let mut previous = 0;
    for span in spans {
        if span.range.start < previous || span.range.start > span.range.end || span.range.end > len
        {
            return Err(LayoutError("face spans must be sorted and disjoint"));
        }
        previous = span.range.end;
    }
    let row_limit = ((spec.viewport.height + spec.vscroll) / font.line_height()).ceil() as usize;
    // Hard row cap makes absurd finite geometry harmless.
    let row_limit = row_limit.saturating_add(spec.overscan_rows).min(4096);
    if !(spec.viewport.x - spec.hscroll).is_finite()
        || !(spec.viewport.y - spec.vscroll).is_finite()
        || !(spec.viewport.y + font.line_height() * (row_limit as f32 + 1.0)).is_finite()
        || !(font.advance * f32::from(spec.tab_width.max(8))).is_finite()
    {
        return Err(LayoutError("layout geometry overflow"));
    }
    let mut rows = Vec::new();
    let mut clusters = Vec::new();
    let mut position = spec.anchor;
    let mut row_start = position;
    let mut scanned = 0;
    let mut logical_column = 0usize;
    let mut x = spec.viewport.x - spec.hscroll;
    let mut y = spec.viewport.y - spec.vscroll;
    let mut continued = false;
    let mut span_index = spans.partition_point(|span| span.range.end <= position);
    let mut characters = snapshot.chars_from(position).peekable();
    let mut exhausted = false;
    while rows.len() < row_limit {
        let Some(&character) = characters.peek() else {
            if position != len {
                return Err(LayoutError("snapshot iterator ended before declared length"));
            }
            push_row(
                &mut rows,
                &mut clusters,
                row_start..position,
                y,
                continued,
                false,
                spec,
                font,
            );
            break;
        };
        if scanned >= spec.scan_budget {
            exhausted = position < len;
            break;
        }
        if position >= len {
            return Err(LayoutError("snapshot iterator exceeds declared length"));
        }
        if character == '\n' {
            characters.next();
            scanned += 1;
            push_row(
                &mut rows,
                &mut clusters,
                row_start..position,
                y,
                continued,
                false,
                spec,
                font,
            );
            position += 1;
            row_start = position;
            logical_column = 0;
            continued = false;
            y += font.line_height();
            x = spec.viewport.x - spec.hscroll;
            continue;
        }
        let (text, cells) = if character == '\t' {
            let count = usize::from(spec.tab_width) - logical_column % usize::from(spec.tab_width);
            (" ".repeat(count), count)
        } else if character.is_ascii_control() {
            let escaped =
                if character == '\u{7f}' { '?' } else { char::from((character as u8) + 64) };
            (format!("^{escaped}"), 2)
        } else {
            let width = metrics.cells(character);
            if width > 8 {
                return Err(LayoutError("unreasonable glyph cell width"));
            }
            (character.to_string(), usize::from(width))
        };
        let width = font.advance * cells as f32;
        if spec.wrap && !clusters.is_empty() && x + width > spec.viewport.x + spec.viewport.width {
            push_row(
                &mut rows,
                &mut clusters,
                row_start..position,
                y,
                continued,
                false,
                spec,
                font,
            );
            row_start = position;
            continued = true;
            y += font.line_height();
            x = spec.viewport.x - spec.hscroll;
            continue;
        }
        while span_index < spans.len() && spans[span_index].range.end <= position {
            span_index += 1;
        }
        let face = spans
            .get(span_index)
            .filter(|span| span.range.contains(&position))
            .map_or(spec.default_face, |span| span.face);
        clusters.push(Cluster {
            source: position..position + 1,
            bounds: Rect { x, y, width, height: font.line_height() },
            text: text.into(),
            face,
        });
        characters.next();
        position += 1;
        scanned += 1;
        logical_column = logical_column.saturating_add(cells);
        x += width;
        if !spec.wrap && x >= spec.viewport.x + spec.viewport.width {
            let next = snapshot.next_line_start(position.saturating_sub(1));
            let line_end = next.map_or(len, |start| start.saturating_sub(1));
            if line_end < position || line_end > len || next.is_some_and(|p| p > len) {
                return Err(LayoutError("invalid snapshot newline index"));
            }
            push_row(
                &mut rows,
                &mut clusters,
                row_start..line_end,
                y,
                continued,
                position < line_end,
                spec,
                font,
            );
            position = next.unwrap_or(len);
            row_start = position;
            logical_column = 0;
            continued = false;
            if next.is_none() {
                break;
            }
            characters = snapshot.chars_from(position).peekable();
            y += font.line_height();
            x = spec.viewport.x - spec.hscroll;
        }
    }
    if !clusters.is_empty() && rows.len() < row_limit {
        push_row(&mut rows, &mut clusters, row_start..position, y, continued, true, spec, font);
    }
    let mut selection = Vec::new();
    for row in &rows {
        for cluster in &row.clusters {
            if spec
                .selection
                .as_ref()
                .is_some_and(|r| cluster.source.start < r.end && cluster.source.end > r.start)
            {
                if let Some(rect) = cluster.bounds.intersection(spec.viewport) {
                    selection.push(rect);
                }
            }
        }
    }
    let cursor = rows.iter().rev().find_map(|row| {
        let candidate = row
            .clusters
            .iter()
            .find(|cluster| cluster.source.start == spec.point)
            .map(|cluster| cluster.bounds.x)
            .or_else(|| {
                (row.source.end == spec.point && !row.truncated).then(|| {
                    row.clusters
                        .last()
                        .map_or(spec.viewport.x - spec.hscroll, |c| c.bounds.x + c.bounds.width)
                })
            });
        candidate
            .and_then(|x| {
                Rect { x, y: row.bounds.y, width: 2.0, height: font.line_height() }
                    .intersection(spec.viewport)
            })
            .map(|bounds| Cursor { position: spec.point, bounds, color: Color(255, 205, 100, 255) })
    });
    Ok(WindowDisplay {
        window_id: spec.window_id,
        buffer_id: spec.buffer_id,
        revision: snapshot.revision(),
        viewport: spec.viewport,
        background: spec.default_face.background,
        rows,
        selection,
        cursor,
        next_anchor: position,
        scanned_chars: scanned,
        budget_exhausted: exhausted,
    })
}
fn push_row(
    rows: &mut Vec<Arc<DisplayRow>>,
    clusters: &mut Vec<Cluster>,
    source: Range<usize>,
    y: f32,
    continued: bool,
    truncated: bool,
    spec: &WindowSpec,
    font: FontMetrics,
) {
    rows.push(Arc::new(DisplayRow {
        source,
        bounds: Rect {
            x: spec.viewport.x,
            y,
            width: spec.viewport.width,
            height: font.line_height(),
        },
        baseline: y + font.ascent,
        continued,
        truncated,
        clusters: std::mem::take(clusters),
    }));
}

/// Retain immutable row Arcs across cursor-only frames and unchanged visible
/// text. Cache size is bounded by window count; geometry/face changes naturally
/// invalidate equality. This is not a replacement for the planned layout tree.
pub struct LayoutCache {
    windows: Vec<WindowDisplay>,
    capacity: usize,
}
impl LayoutCache {
    pub fn new(capacity: usize) -> Self {
        Self { windows: Vec::new(), capacity }
    }
    pub fn layout(
        &mut self,
        snapshot: &dyn TextSnapshot,
        spec: &WindowSpec,
        spans: &[FaceSpan],
        metrics: &dyn Metrics,
    ) -> Result<WindowDisplay, LayoutError> {
        let mut next = layout_window(snapshot, spec, spans, metrics)?;
        if let Some(previous) = self
            .windows
            .iter()
            .find(|w| w.window_id == spec.window_id && w.buffer_id == spec.buffer_id)
        {
            for (new, old) in next.rows.iter_mut().zip(&previous.rows) {
                if new.as_ref() == old.as_ref() {
                    *new = old.clone();
                }
            }
        }
        self.windows.retain(|w| w.window_id != spec.window_id);
        if self.capacity > 0 {
            if self.windows.len() == self.capacity {
                self.windows.remove(0);
            }
            self.windows.push(next.clone());
        }
        Ok(next)
    }
    pub fn retain_windows(&mut self, ids: &[u64]) {
        self.windows.retain(|w| ids.contains(&w.window_id));
    }
}
