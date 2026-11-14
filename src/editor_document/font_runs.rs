//! Opt-in owner font requests and plain native adapter; no Lisp crosses threads.
use super::*;
use rune_editor::display::MeasuredFontCapabilities;
use rune_editor::{Document, EditorHost};
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OwnerFontRunSnapshot {
    pub window: rune_editor::WindowId,
    pub buffer: rune_editor::BufferId,
    pub publication_epoch: u64,
    pub owner_sequence: u64,
    pub frame_id: u64,
    pub visible: Range<usize>,
    pub capabilities: MeasuredFontCapabilities,
    pub runs: Vec<crate::face_attributes::font_runs::FontVariantRun>,
}
impl LispRuntimeHost<'_, '_, '_> {
    pub(crate) fn font_variant_runs(
        &self,
        window: rune_editor::WindowId,
        buffer: rune_editor::BufferId,
        len: usize,
        visible: Range<usize>,
        capabilities: MeasuredFontCapabilities,
    ) -> rune_editor::Result<OwnerFontRunSnapshot> {
        let snapshot = self.owner.snapshot()?;
        let view = snapshot
            .views
            .iter()
            .find(|v| v.id == window && v.view.buffer == buffer)
            .ok_or(rune_editor::EditorError::InvalidWindow)?;
        let buffer_snapshot = snapshot
            .buffers
            .iter()
            .find(|b| b.id == view.view.buffer)
            .ok_or(rune_editor::EditorError::InvalidBuffer)?;
        if buffer_snapshot.document.len_chars() != len
            || visible.start > visible.end
            || visible.end > len
        {
            return Err(rune_editor::EditorError::InvalidPosition);
        }
        let publication_epoch = buffer_snapshot.version;
        let document = buffer_snapshot.document.clone();
        let owner_sequence = snapshot.sequence;
        drop(snapshot); // retain only the authenticated document/property leases
        let frame =
            self.owner.cx.selected_frame().map_err(|_| {
                rune_editor::EditorError::Backend("no live frame for font runs".into())
            })?;
        frame.validate(self.owner.cx).map_err(|_| {
            rune_editor::EditorError::Backend("invalid owner frame for font runs".into())
        })?;
        let runs = crate::face_attributes::font_runs::resolve(
            &document,
            visible.clone(),
            capabilities,
            self.env,
            self.owner.cx,
        )
        .map_err(|e| rune_editor::EditorError::Backend(e.into()))?;
        Ok(OwnerFontRunSnapshot {
            window,
            buffer,
            publication_epoch,
            owner_sequence,
            frame_id: frame.id(),
            visible,
            capabilities,
            runs,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rune_editor::display::{FixedMetrics, MeasuredFontVariant as V};
    fn capture(
        host: &LispRuntimeHost<'_, '_, '_>,
        caps: MeasuredFontCapabilities,
    ) -> rune_editor::Result<OwnerFontRunSnapshot> {
        let s = host.snapshot()?;
        let v = s.views.iter().find(|v| v.id == s.selected_window).unwrap();
        let b = s.buffers.iter().find(|b| b.id == v.view.buffer).unwrap();
        host.font_variant_runs(
            v.id,
            b.id,
            b.document.len_chars(),
            0..b.document.len_chars(),
            caps,
        )
    }
    fn caps() -> MeasuredFontCapabilities {
        MeasuredFontCapabilities {
            bank_id: 1,
            metrics: FixedMetrics::default().0,
            normal: true,
            bold: true,
            italic: true,
            bold_italic: true,
        }
    }
    #[test]
    fn owner_font_requests_inherit_with_explicit_normal_and_mutation_snapshot() {
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"abcd\") (internal-make-lisp-face 'default (selected-frame)) (internal-make-lisp-face 'font-parent (selected-frame)) (internal-set-lisp-face-attribute 'font-parent :weight 'bold) (internal-make-lisp-face 'font-child (selected-frame)) (internal-set-lisp-face-attribute 'font-child :slant 'italic) (internal-set-lisp-face-attribute 'font-child :inherit 'font-parent) (put-text-property 1 3 'face 'font-child) (put-text-property 3 5 'face '(:weight normal :slant italic :inherit font-parent)) (garbage-collect)").map_err(anyhow::Error::msg)?;
            let before=capture(&host,caps())?;
            assert_eq!(before.runs.iter().map(|r|(r.range.clone(),r.variant)).collect::<Vec<_>>(),vec![(0..2,V::BoldItalic),(2..4,V::Italic)]);
            assert!(capture(&host,MeasuredFontCapabilities{bold_italic:false,..caps()}).is_err());
            assert!(host.evaluate_source("(aset (internal-lisp-face-p 'font-parent (selected-frame)) 5 'normal) (error \"after style change\")").is_err());
            let after=capture(&host,caps())?;
            assert_eq!(before.publication_epoch,after.publication_epoch);assert_eq!(before.owner_sequence,after.owner_sequence);
            assert_ne!(before.runs,after.runs);assert_eq!(after.runs.len(),1);assert_eq!(after.runs[0].variant,V::Italic);
            host.evaluate_source("(garbage-collect)").map_err(anyhow::Error::msg)?;
            assert_eq!(after,capture(&host,caps())?);
            Ok(())
        }).unwrap();
    }
    #[test]
    fn unsupported_font_attributes_and_cycles_fail_without_poisoning_correction() {
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"ab\") (internal-make-lisp-face 'default (selected-frame)) (put-text-property 1 3 'face '(:weight semibold))").map_err(anyhow::Error::msg)?;
            assert!(capture(&host,caps()).is_err());
            host.evaluate_source("(put-text-property 1 3 'face '(:weight bold :family \"Other\"))").map_err(anyhow::Error::msg)?;assert!(capture(&host,caps()).is_err());
            host.evaluate_source("(setq font-cycle (list :weight 'bold)) (setcdr (cdr font-cycle) font-cycle) (put-text-property 1 3 'face font-cycle)").map_err(anyhow::Error::msg)?;assert!(capture(&host,caps()).is_err());
            host.evaluate_source("(put-text-property 1 3 'face '(:weight normal :slant normal)) (garbage-collect)").map_err(anyhow::Error::msg)?;
            let fixed=capture(&host,caps())?;assert_eq!(fixed.runs[0].variant,V::Normal);
            let bad=MeasuredFontCapabilities{bank_id:0,..caps()};assert!(capture(&host,bad).is_err());
            assert_eq!(fixed,capture(&host,caps())?);
            Ok(())
        }).unwrap();
    }
    #[test]
    fn owner_font_list_priority_unknown_strings_and_scope_validation() {
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"ab\") (internal-make-lisp-face 'default (selected-frame)) (put-text-property 1 3 'face '((:weight normal) (:weight bold :slant italic)))").map_err(anyhow::Error::msg)?;
            let projected=capture(&host,caps())?;
            assert_eq!(projected.runs[0].variant,V::Italic);
            assert!(host.font_variant_runs(projected.window,projected.buffer,3,0..2,caps()).is_err());
            assert!(host.font_variant_runs(projected.window,projected.buffer,2,0..3,caps()).is_err());
            host.evaluate_source("(internal-make-lisp-face nil (selected-frame)) (internal-set-lisp-face-attribute nil :weight 'bold) (put-text-property 1 3 'face \"not-interned-font-face-xyz\")").map_err(anyhow::Error::msg)?;
            assert!(capture(&host,caps()).is_err());
            host.evaluate_source("(put-text-property 1 3 'face \"nil\")").map_err(anyhow::Error::msg)?;
            assert_eq!(capture(&host,caps())?.runs[0].variant,V::Bold);
            host.evaluate_source("(internal-set-lisp-face-attribute 'default :family \"Other\")").map_err(anyhow::Error::msg)?;
            assert!(capture(&host,caps()).is_err());

            Ok(())
        }).unwrap();
    }
    #[test]
    fn nonnil_font_lock_face_fails_closed_even_alongside_explicit_normal_face() {
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"ab\") (internal-make-lisp-face 'default (selected-frame)) (put-text-property 1 3 'face '(:weight normal)) (put-text-property 1 3 'font-lock-face '(:weight bold))").map_err(anyhow::Error::msg)?;
            let error=capture(&host,caps()).unwrap_err();
            assert!(error.to_string().contains("font-lock-face font publication unsupported"));
            host.evaluate_source("(put-text-property 1 3 'font-lock-face nil) (garbage-collect)").map_err(anyhow::Error::msg)?;
            assert_eq!(capture(&host,caps())?.runs[0].variant,V::Normal);
            host.evaluate_source("(setq lock-cycle (list nil)) (setcdr lock-cycle lock-cycle) (put-text-property 1 3 'font-lock-face lock-cycle)").map_err(anyhow::Error::msg)?;
            assert!(capture(&host,caps()).is_err());
            host.evaluate_source("(put-text-property 1 3 'font-lock-face nil) (garbage-collect)").map_err(anyhow::Error::msg)?;
            assert_eq!(capture(&host,caps())?.runs[0].variant,V::Normal);
            Ok(())
        }).unwrap();
    }
}

/// Native transport adapter: only plain scoped DTOs cross this function boundary.
#[cfg(feature="native")]
pub(crate) fn transport_projection(host:&LispRuntimeHost<'_,'_,'_>,window:rune_editor::WindowId,buffer:rune_editor::BufferId,len:usize,visible:Range<usize>,capabilities:MeasuredFontCapabilities)->Result<rune_gpui::font_transport::FontRunProjection,String>{
    let p=host.font_variant_runs(window,buffer,len,visible,capabilities).map_err(|e|e.to_string())?;
    Ok(rune_gpui::font_transport::FontRunProjection{window_id:p.window.as_u64(),buffer_id:p.buffer.as_u64(),publication_epoch:p.publication_epoch,owner_sequence:p.owner_sequence,frame_id:p.frame_id,visible:p.visible,capabilities:p.capabilities,runs:p.runs.into_iter().map(|r|rune_gpui::font_transport::ProjectedFontRun{range:r.range,variant:r.variant}).collect()})
}
