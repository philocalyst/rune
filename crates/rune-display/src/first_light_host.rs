//! Minimal single-window fixture host, not Emacs window policy or an editor.
use crate::*;
use std::sync::Arc;

pub struct FirstLightHost {
    snapshot: Arc<dyn TextSnapshot>,
    spec: WindowSpec,
    metrics: FixedMetrics,
    frame: Arc<FrameDisplay>,
    inset: f32,
    scale: f32,
}
impl FirstLightHost {
    pub fn new(
        snapshot: Arc<dyn TextSnapshot>,
        spec: WindowSpec,
        metrics: FixedMetrics,
        bounds: Rect,
        inset: f32,
    ) -> Result<Self, LayoutError> {
        if !bounds.valid() || !inset.is_finite() || inset < 0.0 {
            return Err(LayoutError("invalid host geometry"));
        }
        let frame = Arc::new(FrameDisplay {
            protocol_version: DISPLAY_PROTOCOL_VERSION,
            generation: 1,
            bounds,
            background: spec.default_face.background,
            windows: vec![layout_window(&*snapshot, &spec, &[], &metrics)?],
        });
        Ok(Self { snapshot, spec, metrics, frame, inset, scale: 1.0 })
    }
    pub fn frame(&self) -> Arc<FrameDisplay> {
        self.frame.clone()
    }
    /// Resize and left-click cursor placement only. Other events deliberately
    /// do not pretend to edit or scroll an interpreter buffer.
    pub fn apply(&mut self, event: &InputEvent) -> Result<bool, LayoutError> {
        let mut spec = self.spec.clone();
        let mut bounds = self.frame.bounds;
        let mut scale = self.scale;
        match *event {
            InputEvent::Resize { width, height, scale: next_scale } => {
                if ![width, height, next_scale].iter().all(|v| v.is_finite() && *v > 0.0) {
                    return Err(LayoutError("invalid host resize"));
                }
                if width == bounds.width && height == bounds.height && scale == next_scale {
                    return Ok(false);
                }
                bounds.width = width;
                bounds.height = height;
                scale = next_scale;
                // Preserve a positive viewport even for tiny OS windows.
                let x = self.inset.min(width / 4.0);
                let y = self.inset.min(height / 4.0);
                spec.viewport = Rect {
                    x: bounds.x + x,
                    y: bounds.y + y,
                    width: width - 2.0 * x,
                    height: height - 2.0 * y,
                };
            }
            InputEvent::PointerHit { hit, button: 1 } => {
                if hit.window_id != spec.window_id
                    || hit.buffer_id != spec.buffer_id
                    || hit.revision != self.snapshot.revision()
                    || hit.position > self.snapshot.len_chars()
                {
                    return Err(LayoutError("stale or foreign pointer hit"));
                }
                if spec.point == hit.position {
                    return Ok(false);
                }
                spec.point = hit.position;
            }
            _ => return Ok(false),
        }
        let generation = self
            .frame
            .generation
            .checked_add(1)
            .ok_or(LayoutError("display generation overflow"))?;
        let frame = Arc::new(FrameDisplay {
            protocol_version: DISPLAY_PROTOCOL_VERSION,
            generation,
            bounds,
            background: spec.default_face.background,
            windows: vec![layout_window(&*self.snapshot, &spec, &[], &self.metrics)?],
        });
        self.frame = frame;
        self.spec = spec;
        self.scale = scale;
        Ok(true)
    }
}
