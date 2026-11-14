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

// GNU xfaces.c keeps normalized copies in these C-defined variables. Keep the
// rooted Lisp values authoritative, rather than a stale duplicate native alist.
defvar!(FACE_ALTERNATIVE_FONT_FAMILY_ALIST);
defvar!(FACE_ALTERNATIVE_FONT_REGISTRY_ALIST);
const MAX_ALTERNATIVE_NAME_BYTES: usize = 64 * 1024;
const MAX_ALTERNATIVE_BYTES: usize = 16 * 1024 * 1024;
fn charge_name(name: &str, total: &mut usize) -> Result<()> {
    ensure!(
        name.len() <= MAX_ALTERNATIVE_NAME_BYTES,
        "font alternative name byte limit exceeded"
    );
    let next = total
        .checked_add(name.len())
        .ok_or_else(|| anyhow!("font alternative byte count overflow"))?;
    ensure!(next <= MAX_ALTERNATIVE_BYTES, "font alternative aggregate byte limit exceeded");
    *total = next;
    Ok(())
}
fn string_rows(alist: Object) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut bytes = 0;
    let mut tail = alist;
    let mut seen = std::collections::HashSet::new();
    while !tail.is_nil() {
        let cell =
            tail.cons().ok_or_else(|| anyhow!("font alternatives require a proper alist"))?;
        ensure!(seen.insert(std::ptr::from_ref(cell) as usize), "cyclic font alternatives");
        ensure!(rows.len() < 4096, "font alternative row limit exceeded");
        let mut row = Vec::new();
        let mut entry = cell.car();
        let mut entry_seen = std::collections::HashSet::new();
        while !entry.is_nil() {
            let part = entry
                .cons()
                .ok_or_else(|| anyhow!("font alternatives require proper string rows"))?;
            ensure!(
                entry_seen.insert(std::ptr::from_ref(part) as usize),
                "cyclic font alternative row"
            );
            ensure!(row.len() < 256, "font alternative name limit exceeded");
            let value = part.car();
            let name: &str = value.try_into()?;
            charge_name(name, &mut bytes)?;
            row.push(name.to_owned());
            entry = part.cdr();
        }
        rows.push(row);
        tail = cell.cdr();
    }
    Ok(rows)
}
fn commit_alternatives<'a>(
    rows: Vec<Vec<String>>,
    families: bool,
    env: &mut Rt<Env>,
    cx: &'a crate::core::gc::Context,
) -> Result<Object<'a>> {
    use crate::core::{cons::Cons, env::sym};
    let mut normalized = NIL;
    for row in rows.iter().rev() {
        let mut entry = NIL;
        for name in row.iter().rev() {
            let value = if families {
                crate::lread::intern(name, cx).into()
            } else {
                cx.add(name.to_ascii_lowercase())
            };
            entry = Cons::new(value, entry, cx).into();
        }
        normalized = Cons::new(entry, normalized, cx).into();
    }
    let generation = env
        .font_selection
        .generation
        .checked_add(1)
        .ok_or_else(|| anyhow!("font selection generation exhausted"))?;
    env.vars.try_reserve(1)?;
    env.vars.insert(
        if families {
            sym::FACE_ALTERNATIVE_FONT_FAMILY_ALIST
        } else {
            sym::FACE_ALTERNATIVE_FONT_REGISTRY_ALIST
        },
        normalized,
    );
    env.font_selection.generation = generation;
    env.font_selection.cached = None;
    Ok(normalized)
}
#[defun]
fn internal_set_alternative_font_family_alist<'a>(
    alist: Object,
    env: &mut Rt<Env>,
    cx: &'a crate::core::gc::Context,
) -> Result<Object<'a>> {
    commit_alternatives(string_rows(alist)?, true, env, cx)
}
#[defun]
fn internal_set_alternative_font_registry_alist<'a>(
    alist: Object,
    env: &mut Rt<Env>,
    cx: &'a crate::core::gc::Context,
) -> Result<Object<'a>> {
    let rows = string_rows(alist)?;
    // Registry encodings are ASCII identifiers. Do not silently approximate
    // Unicode case tables or manufacture an unsupported charset match.
    ensure!(
        rows.iter().flatten().all(|name| name.is_ascii()),
        "non-ASCII font registry alternatives unsupported"
    );
    commit_alternatives(rows, false, env, cx)
}
/// Consult the authoritative normalized owner alist on each resolution, so
/// subsequent Lisp list mutation cannot leave a duplicate native map stale.
pub(crate) fn resolve_family(
    requested: &str,
    env: &Rt<Env>,
    cx: &crate::core::gc::Context,
    mut available: impl FnMut(&str) -> bool,
) -> Result<Option<String>> {
    use crate::core::env::sym;
    let mut bytes = 0;
    charge_name(requested, &mut bytes)?;
    let mut rows = 0;
    if available(requested) {
        return Ok(Some(requested.to_owned()));
    }
    let mut tail = env
        .vars
        .get(sym::FACE_ALTERNATIVE_FONT_FAMILY_ALIST)
        .map(|v| v.bind(cx))
        .unwrap_or(NIL);
    let mut seen = std::collections::HashSet::new();
    while !tail.is_nil() {
        ensure!(rows < 4096, "font family lookup row limit exceeded");
        rows += 1;
        let cell = tail.cons().ok_or_else(|| anyhow!("invalid normalized font family alist"))?;
        ensure!(
            seen.insert(std::ptr::from_ref(cell) as usize),
            "cyclic normalized font family alist"
        );
        let row = cell.car();
        if let Some(head) = row.cons() {
            let first = head.car();
            let ObjectType::Symbol(name) = first.untag() else {
                return Err(anyhow!("invalid normalized font family name"));
            };
            charge_name(name.name(), &mut bytes)?;
            if name.interned() && name.name() == requested {
                let mut entry = head.cdr();
                let mut inner = std::collections::HashSet::new();
                let mut names = 0;
                while !entry.is_nil() {
                    ensure!(names < 256, "font family lookup name limit exceeded");
                    names += 1;
                    let c = entry
                        .cons()
                        .ok_or_else(|| anyhow!("invalid normalized font family row"))?;
                    ensure!(
                        inner.insert(std::ptr::from_ref(c) as usize),
                        "cyclic normalized font family row"
                    );
                    let value = c.car();
                    let ObjectType::Symbol(name) = value.untag() else {
                        return Err(anyhow!("invalid normalized font family alternative"));
                    };
                    charge_name(name.name(), &mut bytes)?;
                    if available(name.name()) {
                        return Ok(Some(name.name().to_owned()));
                    }
                    entry = c.cdr();
                }
                return Ok(None);
            }
        }
        tail = cell.cdr();
    }
    Ok(None)
}
#[cfg(test)]
mod alternative_tests {
    use super::*;
    #[test]
    fn byte_limits_precede_copy_and_invalid_update_preserves_owner_metadata() {
        use crate::core::{
            cons::Cons,
            env::sym,
            gc::{Context, RootSet},
        };
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        let long = cx.add("x".repeat(MAX_ALTERNATIVE_NAME_BYTES + 1));
        let row = Cons::new(long, NIL, cx);
        let alist = Cons::new(row, NIL, cx);
        assert!(internal_set_alternative_font_family_alist(alist.into(), env, cx).is_err());
        assert_eq!(env.font_selection.generation, 0);
        let name = cx.add("x".repeat(MAX_ALTERNATIVE_NAME_BYTES));
        let mut outer = NIL;
        for _ in 0..4 {
            let mut inner = NIL;
            for _ in 0..65 {
                inner = Cons::new(name, inner, cx).into();
            }
            outer = Cons::new(inner, outer, cx).into();
        }
        assert!(internal_set_alternative_font_family_alist(outer, env, cx).is_err());
        assert_eq!(env.font_selection.generation, 0);
        assert!(env.vars.get(sym::FACE_ALTERNATIVE_FONT_FAMILY_ALIST).is_none());
        assert!(
            resolve_family(&"x".repeat(MAX_ALTERNATIVE_NAME_BYTES + 1), env, cx, |_| panic!(
                "oversized request must not reach resolver"
            ))
            .is_err()
        );
    }
    #[test]
    fn family_resolution_work_is_bounded_after_direct_lisp_mutation() {
        use crate::core::{
            cons::Cons,
            env::sym,
            gc::{Context, RootSet},
        };
        use rune_core::macros::root;
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        let name = crate::lread::intern("not-requested", cx);
        let row = Cons::new(name, NIL, cx);
        let mut alist = NIL;
        for _ in 0..4097 {
            alist = Cons::new(row, alist, cx).into();
        }
        env.vars.insert(sym::FACE_ALTERNATIVE_FONT_FAMILY_ALIST, alist);
        assert!(resolve_family("requested", env, cx, |_| false).is_err());
    }
    #[test]
    fn normalization_return_and_input_copy_match_gnu() {
        for source in [
            "(let ((x '((\"Mono\" \"Courier\" \"fixed\")))) (list (internal-set-alternative-font-family-alist x) x))",
            "(let ((x '((\"ISO8859-1\" \"ASCII\")))) (list (internal-set-alternative-font-registry-alist x) x))",
        ] {
            crate::assert_elprop!("{}", source);
        }
    }
    #[test]
    fn real_family_fallback_uses_owner_metadata_and_invalid_update_is_atomic() {
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
            "(internal-set-alternative-font-family-alist '((\"Mono\" \"Courier\" \"fixed\")))",
            cx,
            env,
        )
        .unwrap();
        assert_eq!(
            resolve_family("Mono", env, cx, |s| s == "fixed").unwrap(),
            Some("fixed".into())
        );
        let generation = env.font_selection.generation;
        assert!(
            crate::lread::load_internal(
                "(internal-set-alternative-font-family-alist '((\"Mono\" 1)))",
                cx,
                env
            )
            .is_err()
        );
        assert_eq!(env.font_selection.generation, generation);
        assert_eq!(
            resolve_family("Mono", env, cx, |s| s == "fixed").unwrap(),
            Some("fixed".into())
        );
    }
}
