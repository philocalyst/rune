//! Owner-side font selection metadata. No Lisp values cross the renderer seam.
//! GNU faces.el owns face-spec-set; this module implements observed C primitives.
use crate::core::{
    env::Env,
    gc::Rt,
    object::{NIL, Object, ObjectType},
};
use anyhow::{Result, anyhow, ensure};
use rune_macros::defun;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontAttribute {
    Width,
    Height,
    Weight,
    Slant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FontDistance {
    pub width: u32,
    pub height: u32,
    pub weight: u32,
    pub slant: u32,
}
impl FontDistance {
    fn at(self, attribute: FontAttribute) -> u32 {
        match attribute {
            FontAttribute::Width => self.width,
            FontAttribute::Height => self.height,
            FontAttribute::Weight => self.weight,
            FontAttribute::Slant => self.slant,
        }
    }
}
#[derive(Debug)]
pub(crate) struct FontSelection {
    order: [FontAttribute; 4],
    generation: u64,
    cached: Option<(Vec<FontDistance>, usize)>,
}
impl Default for FontSelection {
    fn default() -> Self {
        Self {
            order: [
                FontAttribute::Width,
                FontAttribute::Height,
                FontAttribute::Weight,
                FontAttribute::Slant,
            ],
            generation: 0,
            cached: None,
        }
    }
}
impl FontSelection {
    pub(crate) fn set_order(&mut self, order: [FontAttribute; 4]) -> Result<()> {
        if self.order == order {
            return Ok(());
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("font selection generation exhausted"))?;
        self.order = order;
        self.generation = generation;
        self.cached = None;
        Ok(())
    }
    /// Real lexicographic font-candidate matching. Distances are supplied by a
    /// platform font resolver; ties retain resolver order. Fixed-font GPUI has
    /// no candidate publication path yet and must not claim face-font parity.
    pub(crate) fn best_match(&mut self, candidates: &[FontDistance]) -> Option<usize> {
        if let Some((old, index)) = &self.cached {
            if old == candidates {
                return Some(*index);
            }
        }
        let result = (0..candidates.len()).min_by(|a, b| {
            for attribute in self.order {
                let c = candidates[*a].at(attribute).cmp(&candidates[*b].at(attribute));
                if !c.is_eq() {
                    return c;
                }
            }
            a.cmp(b)
        });
        self.cached = result.map(|index| (candidates.to_vec(), index));
        result
    }
}
#[defun]
fn internal_set_font_selection_order(order: Object, env: &mut Rt<Env>) -> Result<()> {
    // GNU 30.1 src/xfaces.c accepts exactly four recognized attributes and
    // (despite its docstring) returns nil. It also accepts duplicate attributes.
    let mut tail = order;
    let mut result = [FontAttribute::Width; 4];
    for item in &mut result {
        let cell = tail.cons().ok_or_else(|| anyhow!("Invalid font sort order"))?;
        let value = cell.car();
        let ObjectType::Symbol(symbol) = value.untag() else {
            return Err(anyhow!("Invalid font sort order"));
        };
        ensure!(symbol.interned(), "Invalid font sort order");
        *item = match symbol.name() {
            ":width" => FontAttribute::Width,
            ":height" => FontAttribute::Height,
            ":weight" => FontAttribute::Weight,
            ":slant" => FontAttribute::Slant,
            _ => return Err(anyhow!("Invalid font sort order")),
        };
        tail = cell.cdr();
    }
    ensure!(tail == NIL, "Invalid font sort order");
    env.font_selection.set_order(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn font_priority_changes_real_candidate_choice_and_invalidates_cache_atomically() {
        let candidates = [
            FontDistance { width: 0, height: 20, weight: 0, slant: 0 },
            FontDistance { width: 0, height: 0, weight: 30, slant: 0 },
        ];
        let mut policy = FontSelection::default();
        assert_eq!(policy.best_match(&candidates), Some(1));
        assert!(policy.cached.is_some());
        policy
            .set_order([
                FontAttribute::Weight,
                FontAttribute::Width,
                FontAttribute::Height,
                FontAttribute::Slant,
            ])
            .unwrap();
        assert_eq!(policy.generation, 1);
        assert!(policy.cached.is_none());
        assert_eq!(policy.best_match(&candidates), Some(0));
        policy.generation = u64::MAX;
        let old = policy.order;
        assert!(policy.set_order([FontAttribute::Width; 4]).is_err());
        assert_eq!(policy.order, old);
        assert!(policy.cached.is_some());
    }
    #[test]
    fn observed_font_order_primitive_matches_gnu_including_duplicates_and_nil_return() {
        for source in [
            "(internal-set-font-selection-order '(:width :height :weight :slant))",
            "(internal-set-font-selection-order '(:width :width :width :width))",
            "(internal-set-font-selection-order '(:slant :weight :height :width))",
        ] {
            crate::assert_elprop!("{}", source);
        }
    }
    #[test]
    fn invalid_font_order_does_not_mutate_policy_or_cache() {
        use crate::core::{
            env::sym,
            gc::{Context, RootSet},
        };
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::lread::load_internal(
            "(internal-set-font-selection-order '(:slant :height :weight :width))",
            cx,
            env,
        )
        .unwrap();
        let before = env.font_selection.order;
        let generation = env.font_selection.generation;
        for source in [
            "(internal-set-font-selection-order nil)",
            "(internal-set-font-selection-order '(:width :height :weight))",
            "(internal-set-font-selection-order '(:bogus :height :weight :slant))",
            "(internal-set-font-selection-order '(:width :height :weight :slant . bad))",
        ] {
            assert!(crate::lread::load_internal(source, cx, env).is_err());
            assert_eq!(env.font_selection.order, before);
            assert_eq!(env.font_selection.generation, generation);
        }
    }
}
