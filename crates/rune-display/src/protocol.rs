use std::{ops::Range, sync::Arc};

pub const DISPLAY_PROTOCOL_VERSION: u32 = 1;
/// Logical pixels, independent of the OS backing scale factor.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Rect {
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.x + self.width && p.y < self.y + self.height
    }
    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let width = (self.x + self.width).min(other.x + other.width) - x;
        let height = (self.y + self.height).min(other.y + other.height) - y;
        (width > 0.0 && height > 0.0).then_some(Self { x, y, width, height })
    }
    pub fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height].iter().all(|v| v.is_finite())
            && self.width > 0.0
            && self.height > 0.0
            && (self.x + self.width).is_finite()
            && (self.y + self.height).is_finite()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Face {
    pub id: u64,
    pub foreground: Color,
    pub background: Color,
}
impl Default for Face {
    fn default() -> Self {
        Self { id: 0, foreground: Color(220, 224, 234, 255), background: Color(24, 28, 36, 255) }
    }
}
/// Prepared by Lisp/editor side; layout never resolves Lisp objects or evaluates specs.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceSpan {
    pub range: Range<usize>,
    pub face: Face,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontMetrics {
    pub advance: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
}
impl FontMetrics {
    pub fn line_height(self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
    pub fn valid(self) -> bool {
        [self.advance, self.ascent, self.descent, self.line_gap]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
            && self.advance > 0.0
            && self.ascent > 0.0
            && self.line_height().is_finite()
    }
}
/// Backend-provided advances. Complex shaping/bidi are deliberately not faked.
pub trait Metrics: Send + Sync {
    fn font_metrics(&self) -> FontMetrics;
    fn cells(&self, character: char) -> u8;
}
#[derive(Debug, Clone, Copy)]
pub struct FixedMetrics(pub FontMetrics);
impl Default for FixedMetrics {
    fn default() -> Self {
        Self(FontMetrics { advance: 8.0, ascent: 13.0, descent: 4.0, line_gap: 3.0 })
    }
}
impl Metrics for FixedMetrics {
    fn font_metrics(&self) -> FontMetrics {
        self.0
    }
    fn cells(&self, _: char) -> u8 {
        1
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub source: Range<usize>,
    pub bounds: Rect,
    pub text: Arc<str>,
    pub face: Face,
}
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRow {
    pub source: Range<usize>,
    pub bounds: Rect,
    pub baseline: f32,
    pub continued: bool,
    pub truncated: bool,
    pub clusters: Vec<Cluster>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub position: usize,
    pub bounds: Rect,
    pub color: Color,
}
#[derive(Debug, Clone, PartialEq)]
pub struct WindowDisplay {
    pub window_id: u64,
    pub buffer_id: u64,
    pub revision: u64,
    pub viewport: Rect,
    pub background: Color,
    pub rows: Vec<Arc<DisplayRow>>,
    pub selection: Vec<Rect>,
    pub cursor: Option<Cursor>,
    pub next_anchor: usize,
    pub scanned_chars: usize,
    pub budget_exhausted: bool,
}
#[derive(Debug, Clone, PartialEq)]
pub struct FrameDisplay {
    pub protocol_version: u32,
    pub generation: u64,
    pub bounds: Rect,
    pub background: Color,
    pub windows: Vec<WindowDisplay>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub window_id: u64,
    pub buffer_id: u64,
    pub revision: u64,
    pub position: usize,
    pub affinity: Affinity,
}
impl FrameDisplay {
    /// Reverse paint order gives topmost window priority. Returned positions are
    /// tagged with revision so a stale UI click can be translated/rejected.
    pub fn hit_test(&self, point: Point) -> Option<Hit> {
        if !self.bounds.contains(point) {
            return None;
        }
        for window in self.windows.iter().rev() {
            if !window.viewport.contains(point) {
                continue;
            }
            let row = window.rows.iter().find(|row| {
                point.y >= row.bounds.y && point.y < row.bounds.y + row.bounds.height
            })?;
            for cluster in &row.clusters {
                if point.x < cluster.bounds.x + cluster.bounds.width {
                    let after = point.x >= cluster.bounds.x + cluster.bounds.width / 2.0;
                    return Some(Hit {
                        window_id: window.window_id,
                        buffer_id: window.buffer_id,
                        revision: window.revision,
                        position: if after { cluster.source.end } else { cluster.source.start },
                        affinity: if after { Affinity::After } else { Affinity::Before },
                    });
                }
            }
            return Some(Hit {
                window_id: window.window_id,
                buffer_id: window.buffer_id,
                revision: window.revision,
                position: row.source.end,
                affinity: Affinity::After,
            });
        }
        None
    }
}
/// A GPUI custom Element or a deterministic headless target implements this.
/// Glyph text is not widget text: coordinates/baselines come from this display list.
pub trait PaintTarget {
    fn push_clip(&mut self, rect: Rect);
    fn pop_clip(&mut self);
    fn fill(&mut self, rect: Rect, color: Color);
    fn text(&mut self, text: &str, origin: Point, face: Face, metrics: FontMetrics);
}
pub fn paint_frame(frame: &FrameDisplay, metrics: FontMetrics, target: &mut impl PaintTarget) {
    target.push_clip(frame.bounds);
    target.fill(frame.bounds, frame.background);
    for window in &frame.windows {
        target.push_clip(window.viewport);
        target.fill(window.viewport, window.background);
        for row in &window.rows {
            target.fill(row.bounds, window.background);
            for cluster in &row.clusters {
                target.fill(cluster.bounds, cluster.face.background);
            }
        }
        for rect in &window.selection {
            target.fill(*rect, Color(66, 99, 160, 150));
        }
        for row in &window.rows {
            for cluster in &row.clusters {
                target.text(
                    &cluster.text,
                    Point { x: cluster.bounds.x, y: row.baseline },
                    cluster.face,
                    metrics,
                );
            }
        }
        if let Some(cursor) = &window.cursor {
            target.fill(cursor.bounds, cursor.color);
        }
        target.pop_clip();
    }
    target.pop_clip();
}
