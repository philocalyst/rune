//! Opt-in UI font admission. No automatic GNU face-weight or owner publication.
use crate::{NativeFont, NativeFontVariant, NativeFontVariantBank};
use rune_display::{FrameDisplay, FrameFontPlan};
use std::{rc::Rc, sync::Arc};

pub struct NativeFrameFontPlan {
    plan: FrameFontPlan,
    bank: Rc<NativeFontVariantBank>,
}
impl NativeFrameFontPlan {
    pub fn new(
        plan: FrameFontPlan,
        base: &NativeFont,
        bank: Rc<NativeFontVariantBank>,
        cx: &gpui::App,
    ) -> Result<Self, &'static str> {
        bank.validate_baseline(base, cx)?;
        bank.get_compatible_font(
            plan.bank_id(),
            plan.metrics(),
            NativeFontVariant::Normal,
            cx,
        )?;
        for assignment in plan.assignments() {
            bank.get_compatible_font(plan.bank_id(), plan.metrics(), assignment.variant, cx)?;
        }
        Ok(Self { plan, bank })
    }
    pub fn frame(&self) -> &Arc<FrameDisplay> {
        self.plan.frame()
    }
    pub fn matches_frame(&self, frame: &Arc<FrameDisplay>) -> bool {
        self.plan.matches_frame(frame)
    }
    pub fn validate_for_base(&self, base: &NativeFont, cx: &gpui::App) -> Result<(), &'static str> {
        self.bank.validate_baseline(base, cx)?;
        self.validate(cx)
    }
    pub fn validate(&self, cx: &gpui::App) -> Result<(), &'static str> {
        self.bank
            .get_compatible_font(
                self.plan.bank_id(),
                self.plan.metrics(),
                NativeFontVariant::Normal,
                cx,
            )
            .map(|_| ())
    }
    pub fn font_for_face(&self, face_id: u64, cx: &gpui::App) -> Result<&gpui::Font, &'static str> {
        self.bank.get_compatible_font(
            self.plan.bank_id(),
            self.plan.metrics(),
            self.plan.variant_for_face(face_id),
            cx,
        )
    }
}
