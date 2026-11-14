//! Optional plain per-face font requests paired with one exact immutable frame.
//! This is not input authority or GNU face-weight publication.
use crate::{DISPLAY_PROTOCOL_VERSION, FontMetrics, FrameDisplay, MeasuredFontVariant};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameFontAssignment {
    pub face_id: u64,
    pub variant: MeasuredFontVariant,
}
#[derive(Clone, Debug)]
pub struct FrameFontPlan {
    frame: Arc<FrameDisplay>,
    bank_id: u64,
    metrics: FontMetrics,
    assignments: Vec<FrameFontAssignment>,
}
impl FrameFontPlan {
    /// Caller owns this bounded plain request. UI admission must additionally
    /// validate actual font-bank lifetime and measured availability.
    pub fn new(
        frame: Arc<FrameDisplay>,
        bank_id: u64,
        metrics: FontMetrics,
        mut assignments: Vec<FrameFontAssignment>,
    ) -> Result<Self, &'static str> {
        if bank_id == 0 || !metrics.valid() {
            return Err("invalid font plan bank or metrics");
        }
        if frame.protocol_version != DISPLAY_PROTOCOL_VERSION {
            return Err("incompatible font plan frame protocol");
        }
        if assignments.len() > 256 {
            return Err("font assignment budget exceeded");
        }
        assignments.sort_unstable_by_key(|a| a.face_id);
        if assignments.windows(2).any(|a| a[0].face_id == a[1].face_id) {
            return Err("duplicate font assignment");
        }
        let mut seen = vec![false; assignments.len()];
        let mut rows = 0usize;
        let mut clusters = 0usize;
        if frame.windows.len() > 256 {
            return Err("font frame window budget exceeded");
        }
        for window in &frame.windows {
            rows = rows
                .checked_add(window.rows.len())
                .ok_or("font frame budget overflow")?;
            if rows > 4096 {
                return Err("font frame row budget exceeded");
            }
            for row in &window.rows {
                clusters = clusters
                    .checked_add(row.clusters.len())
                    .ok_or("font frame budget overflow")?;
                if clusters > 65536 {
                    return Err("font frame cluster budget exceeded");
                }
                for cluster in &row.clusters {
                    if let Ok(i) = assignments.binary_search_by_key(&cluster.face.id, |a| a.face_id)
                    {
                        seen[i] = true;
                    }
                }
            }
        }
        if seen.iter().any(|seen| !*seen) {
            return Err("font assignment face absent from frame");
        }
        Ok(Self {
            frame,
            bank_id,
            metrics,
            assignments,
        })
    }
    pub fn frame(&self) -> &Arc<FrameDisplay> {
        &self.frame
    }
    pub fn bank_id(&self) -> u64 {
        self.bank_id
    }
    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }
    pub fn matches_frame(&self, frame: &Arc<FrameDisplay>) -> bool {
        Arc::ptr_eq(&self.frame, frame)
    }
    pub fn assignments(&self) -> &[FrameFontAssignment] {
        &self.assignments
    }
    pub fn variant_for_face(&self, face_id: u64) -> MeasuredFontVariant {
        self.assignments
            .binary_search_by_key(&face_id, |a| a.face_id)
            .map(|i| self.assignments[i].variant)
            .unwrap_or(MeasuredFontVariant::Normal)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn frame() -> Arc<FrameDisplay> {
        Arc::new(FrameDisplay {
            protocol_version: DISPLAY_PROTOCOL_VERSION,
            generation: 7,
            bounds: crate::Rect {
                x: 0.,
                y: 0.,
                width: 1.,
                height: 1.,
            },
            background: crate::Face::default().background,
            windows: vec![],
        })
    }
    #[test]
    fn exact_frame_identity_is_not_replaceable_by_equal_generation() {
        let frame = frame();
        let other = Arc::new((*frame).clone());
        let plan =
            FrameFontPlan::new(frame.clone(), 2, crate::FixedMetrics::default().0, vec![]).unwrap();
        assert!(plan.matches_frame(&frame));
        assert!(!plan.matches_frame(&other));
        assert_eq!(plan.variant_for_face(42), MeasuredFontVariant::Normal);
        fn plain<T: Send + Sync>() {}
        plain::<FrameFontPlan>();
    }
    #[test]
    fn rejects_invalid_geometry_zero_bank_duplicates_and_unbound_faces() {
        let f = frame();
        let m = crate::FixedMetrics::default().0;
        assert!(FrameFontPlan::new(f.clone(), 0, m, vec![]).is_err());
        assert!(
            FrameFontPlan::new(
                f.clone(),
                1,
                FontMetrics {
                    advance: f32::NAN,
                    ..m
                },
                vec![]
            )
            .is_err()
        );
        let a = FrameFontAssignment {
            face_id: 1,
            variant: MeasuredFontVariant::Bold,
        };
        assert_eq!(
            FrameFontPlan::new(f.clone(), 1, m, vec![a, a]).unwrap_err(),
            "duplicate font assignment"
        );
        assert_eq!(
            FrameFontPlan::new(f.clone(), 1, m, vec![a]).unwrap_err(),
            "font assignment face absent from frame"
        );
        assert_eq!(
            FrameFontPlan::new(f, 1, m, vec![a; 257]).unwrap_err(),
            "font assignment budget exceeded"
        );
    }
    #[test]
    fn explicit_requests_select_only_existing_face_ids_and_bound_frame_work() {
        let mut f = (*frame()).clone();
        let rect = f.bounds;
        let row = Arc::new(crate::DisplayRow {
            source: 0..1,
            bounds: rect,
            baseline: 1.,
            continued: false,
            truncated: false,
            clusters: vec![crate::Cluster {
                source: 0..1,
                bounds: rect,
                text: "x".into(),
                face: crate::Face {
                    id: 5,
                    ..crate::Face::default()
                },
            }],
        });
        let window = crate::WindowDisplay {
            window_id: 1,
            buffer_id: 2,
            revision: 3,
            viewport: rect,
            background: f.background,
            rows: vec![row.clone()],
            selection: vec![],
            cursor: None,
            next_anchor: 1,
            scanned_chars: 1,
            budget_exhausted: false,
        };
        f.windows.push(window);
        let m = crate::FixedMetrics::default().0;
        let plan = FrameFontPlan::new(
            Arc::new(f.clone()),
            1,
            m,
            vec![FrameFontAssignment {
                face_id: 5,
                variant: MeasuredFontVariant::BoldItalic,
            }],
        )
        .unwrap();
        assert_eq!(plan.variant_for_face(5), MeasuredFontVariant::BoldItalic);
        assert_eq!(plan.variant_for_face(6), MeasuredFontVariant::Normal);
        f.windows[0].rows = vec![row; 4097];
        assert_eq!(
            FrameFontPlan::new(Arc::new(f), 1, m, vec![]).unwrap_err(),
            "font frame row budget exceeded"
        );
    }
}
