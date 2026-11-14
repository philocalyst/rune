//! Owner-rooted GNU-layout face vectors and per-real-frame metadata.
use crate::core::{
    cons::Cons,
    env::{Env, sym},
    gc::{Context, Rt},
    object::{Gc, LispFrame, LispHashTable, LispVec, NIL, Object, ObjectType, Symbol, TRUE},
};
use anyhow::{Result, anyhow, ensure};
use rune_macros::defun;
defsym!(FACE);
defsym!(DEFAULT);
defsym!(FACE_ALIAS);
defsym!(UNSPECIFIED);
// GNU Emacs src/emacs.c initializes the X-resource policy switch to false.
// Terminal frame capability, not a forced true flag, avoids X resource calls.
defvar_bool!(INHIBIT_X_RESOURCES, false);
// GNU minibuf.c initializes only the read-only pair; original faces.el adds
// its prompt face later. Prompt field/stickiness are separate input-engine work.
defvar!(MINIBUFFER_PROMPT_PROPERTIES, crate::fns::slice_into_list(&[crate::lread::intern("read-only", cx).into(), crate::core::object::TRUE], None, cx));
defvar!(FACE__NEW_FRAME_DEFAULTS, crate::face_attributes::new_table(cx));
const VECTOR_SIZE: usize = 20;
pub(crate) fn new_table<'a>(cx: &'a Context) -> Object<'a> {
    crate::fns::make_hash_table(&[], cx).expect("empty hash-table options are valid")
}
fn global_table<'a>(env: &mut Rt<Env>, cx: &'a Context) -> Result<&'a LispHashTable> {
    let value = if let Some(value) = env.variable_value(sym::FACE__NEW_FRAME_DEFAULTS, cx)? {
        value
    } else {
        let table = new_table(cx);
        env.set_var_context(sym::FACE__NEW_FRAME_DEFAULTS, table, cx)?;
        table
    };
    value.try_into().map_err(Into::into)
}
fn frame_table<'a>(
    frame: Gc<&'a LispFrame>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<&'a LispHashTable> {
    frame.validate(cx)?;
    let key: Object = frame.into();
    let value = if let Some(value) = env.frame_face_tables.get(key) {
        value.bind(cx)
    } else {
        let table = new_table(cx);
        env.frame_face_tables.try_reserve(1)?;
        env.frame_face_tables.insert(key, table);
        table
    };
    value.try_into().map_err(Into::into)
}
#[defun(name = "frame--face-hash-table")]
fn frame_face_hash_table<'a>(
    frame: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    Ok(frame_table(crate::frame::live_frame(frame, cx)?, env, cx)?.into())
}
fn face_name<'a>(value: Object<'a>, env: &Rt<Env>, cx: &'a Context) -> Result<Option<Symbol<'a>>> {
    let mut face = match value.untag() {
        ObjectType::Symbol(s) => s,
        ObjectType::String(s) => crate::lread::intern(s, cx),
        _ => return Ok(None),
    };
    let alias = crate::lread::intern("face-alias", cx);
    let mut seen = Vec::new();
    for _ in 0..256 {
        ensure!(!seen.contains(&face), "cyclic face alias");
        seen.push(face);
        let next = crate::data::get(face, alias, env, cx);
        if next.is_nil() {
            return Ok(Some(face));
        }
        let ObjectType::Symbol(target) = next.untag() else { return Ok(Some(face)) };
        face = target;
    }
    Err(anyhow!("face alias depth exceeded"))
}
fn checked_vector(value: Object) -> Result<&LispVec> {
    let ObjectType::Vec(vector) = value.untag() else {
        return Err(anyhow!("invalid face vector"));
    };
    ensure!(
        vector.len() == VECTOR_SIZE && vector[0].get() == sym::FACE,
        "invalid face vector"
    );
    Ok(vector)
}
fn global_vector<'a>(
    face: Symbol<'a>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Option<Object<'a>>> {
    let face = face_name(face.into(), env, cx)?.unwrap_or(face);
    match global_table(env, cx)?.get(face.into()) {
        None => Ok(None),
        Some(entry) => {
            let pair = entry.cons().ok_or_else(|| anyhow!("invalid global face entry"))?;
            let value = pair.cdr();
            checked_vector(value)?;
            Ok(Some(value))
        }
    }
}
fn fresh_vector<'a>(cx: &'a Context) -> Object<'a> {
    let mut slots = vec![Object::from(sym::UNSPECIFIED); VECTOR_SIZE];
    slots[0] = sym::FACE.into();
    cx.add(slots)
}
#[defun]
fn internal_make_lisp_face<'a>(
    face: Symbol<'a>,
    frame: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let frame = frame
        .filter(|x| !x.is_nil())
        .map(|f| crate::frame::live_frame(Some(f), cx))
        .transpose()?;
    let global = global_vector(face, env, cx)?;
    let lookup_face = face_name(face.into(), env, cx)?.unwrap_or(face);
    let local = if let Some(frame) = frame {
        frame_table(frame, env, cx)?.get(lookup_face.into())
    } else {
        None
    };
    let existing = if frame.is_some() { local } else { global };
    if let Some(value) = existing {
        checked_vector(value)?.try_mut()?;
    }
    let next_id = if global.is_none() {
        Some(
            env.face_next_id
                .checked_add(1)
                .ok_or_else(|| anyhow!("face identity exhausted"))?,
        )
    } else {
        None
    };
    if let Some(id) = next_id {
        ensure!(id <= i64::MAX as u64, "face identity exhausted");
    }
    let generation = env.font_selection.next_generation()?;
    let global = if let Some(global) = global {
        global
    } else {
        let vector = fresh_vector(cx);
        let id = env.face_next_id;
        let entry = Cons::new(id as i64, vector, cx);
        global_table(env, cx)?.insert(face.into(), entry.into())?;
        env.set_prop(face, sym::FACE, (id as i64).into());
        env.face_next_id = next_id.unwrap();
        vector
    };
    let vector = if let Some(frame) = frame {
        if let Some(value) = local {
            value
        } else {
            let value = fresh_vector(cx);
            frame_table(frame, env, cx)?.insert(face.into(), value)?;
            value
        }
    } else {
        global
    };
    let slots = checked_vector(vector)?.try_mut()?;
    for slot in &slots[1..] {
        slot.set(Object::from(sym::UNSPECIFIED));
    }
    env.font_selection.commit_generation(generation);
    Ok(vector)
}
#[defun]
fn internal_lisp_face_p<'a>(
    face: Object<'a>,
    frame: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let frame = frame
        .filter(|x| !x.is_nil())
        .map(|f| crate::frame::live_frame(Some(f), cx))
        .transpose()?;
    let Some(face) = face_name(face, env, cx)? else { return Ok(NIL) };
    let value = if let Some(frame) = frame {
        frame_table(frame, env, cx)?.get(face.into())
    } else {
        global_vector(face, env, cx)?
    };
    if let Some(value) = value {
        checked_vector(value)?;
        Ok(value)
    } else {
        Ok(NIL)
    }
}
fn attribute_index(attribute: Symbol) -> Result<usize> {
    ensure!(attribute.interned(), "invalid face attribute");
    Ok(match attribute.name() {
        ":family" => 1,
        ":foundry" => 2,
        ":width" => 3,
        ":height" => 4,
        ":weight" => 5,
        ":slant" => 6,
        ":underline" => 7,
        ":inverse-video" => 8,
        ":foreground" => 9,
        ":background" => 10,
        ":stipple" => 11,
        ":overline" => 12,
        ":strike-through" => 13,
        ":box" => 14,
        ":font" => 15,
        ":inherit" => 16,
        ":fontset" => 17,
        ":distant-foreground" => 18,
        ":extend" => 19,
        _ => return Err(anyhow!("invalid face attribute")),
    })
}
fn scoped_vector<'a>(
    face: Symbol<'a>,
    frame: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let value = if frame == Some(TRUE) {
        global_vector(face, env, cx)?
    } else {
        frame_table(crate::frame::live_frame(frame, cx)?, env, cx)?.get(face.into())
    };
    let value = value.ok_or_else(|| anyhow!("Invalid face"))?;
    checked_vector(value)?;
    Ok(value)
}
#[defun]
fn internal_get_lisp_face_attribute<'a>(
    face: Symbol<'a>,
    attribute: Symbol,
    frame: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Object<'a>> {
    let face = face_name(face.into(), env, cx)?.ok_or_else(|| anyhow!("Invalid face"))?;
    let index = attribute_index(attribute)?;
    Ok(checked_vector(scoped_vector(face, frame, env, cx)?)?[index].get())
}
// Validate structured box metadata without evaluating or allocating Lisp data.
// GNU xfaces.c accepts nonzero scalar widths, integer pairs and a keyword list.
// Cyclic/oversized lists fail closed before any face/cache mutation.
fn valid_box(value: Object) -> bool {
    fn integer(value: Object) -> bool { matches!(value.untag(), ObjectType::Int(_)) }
    fn nonzero(value: Object) -> bool { matches!(value.untag(), ObjectType::Int(n) if n != 0) }
    fn nonempty(value: Object) -> bool { matches!(value.untag(), ObjectType::String(s) if !s.is_empty()) }
    if value.is_nil() || nonzero(value) || nonempty(value) { return true; }
    let ObjectType::Cons(pair) = value.untag() else { return false; };
    if integer(pair.car()) && integer(pair.cdr()) { return true; }
    let mut tail = value;
    for _ in 0..256 {
        if tail.is_nil() { return true; }
        let ObjectType::Cons(key_cell) = tail.untag() else { return false; };
        let key = key_cell.car();
        tail = key_cell.cdr();
        // GNU accepts a trailing keyword alone: after taking its car the
        // remaining tail is nil. Preserve that observed representation rule.
        if tail.is_nil() { return true; }
        let ObjectType::Cons(value_cell) = tail.untag() else { return false; };
        let item = value_cell.car();
        let ObjectType::Symbol(key) = key.untag() else { return false; };
        if !key.interned() { return false; }
        let valid = match key.name() {
            ":line-width" => nonzero(item) || matches!(item.untag(), ObjectType::Cons(p) if nonzero(p.car()) && nonzero(p.cdr())),
            ":color" => item.is_nil() || nonempty(item),
            ":style" => item.is_nil() || matches!(item.untag(), ObjectType::Symbol(s) if s.interned() && ["pressed-button", "released-button", "flat-button"].contains(&s.name())),
            _ => false,
        };
        if !valid { return false; }
        tail = value_cell.cdr();
    }
    tail.is_nil()
}
fn validate_attribute<'a>(index: usize, value: Object<'a>) -> Result<Object<'a>> {
    if value == sym::UNSPECIFIED {
        return Ok(value);
    }
    let symbol = match value.untag() {
        ObjectType::Symbol(s) if s.interned() => Some(s),
        _ => None,
    };
    let value = if index == 14 && value == TRUE { 1_i64.into() } else { value };
    let valid = match index {
        1 | 2 | 9 | 10 | 17 | 18 => {
            matches!(value.untag(), ObjectType::String(_)) || value.is_nil()
        }
        3 => {
            symbol.is_some_and(|s| {
                [
                    "ultra-condensed",
                    "extra-condensed",
                    "condensed",
                    "semi-condensed",
                    "normal",
                    "semi-expanded",
                    "expanded",
                    "extra-expanded",
                    "ultra-expanded",
                ]
                .contains(&s.name())
            }) || value.is_nil()
        }
        4 => match value.untag() {
            ObjectType::Int(n) => n > 0,
            ObjectType::Float(n) => **n > 0. && n.is_finite(),
            _ => false,
        },
        5 => {
            symbol.is_some_and(|s| {
                [
                    "ultra-light",
                    "extra-light",
                    "light",
                    "semi-light",
                    "normal",
                    "semi-bold",
                    "bold",
                    "extra-bold",
                    "ultra-bold",
                ]
                .contains(&s.name())
            }) || value.is_nil()
        }
        6 => {
            symbol.is_some_and(|s| {
                ["normal", "italic", "oblique", "reverse-italic", "reverse-oblique"]
                    .contains(&s.name())
            }) || value.is_nil()
        }
        7 | 12 | 13 => {
            value.is_nil() || value == TRUE || matches!(value.untag(), ObjectType::String(_))
        }
        8 | 19 => value.is_nil() || value == TRUE,
        11 => value.is_nil(),
        14 => valid_box(value),
        15 => value.is_nil() || matches!(value.untag(), ObjectType::String(_)),
        16 => value.is_nil() || value.is_symbol() || matches!(value.untag(), ObjectType::Cons(_)),
        _ => false,
    };
    ensure!(valid, "unsupported or invalid face attribute value");
    Ok(if value.is_nil() && matches!(index, 1 | 2 | 3 | 5 | 6 | 9 | 10 | 15 | 17) {
        sym::UNSPECIFIED.into()
    } else {
        value
    })
}
#[defun]
fn internal_set_lisp_face_attribute<'a>(
    face: Symbol<'a>,
    attribute: Symbol,
    value: Object<'a>,
    frame: Option<Object<'a>>,
    env: &mut Rt<Env>,
    cx: &'a Context,
) -> Result<Symbol<'a>> {
    let face = face_name(face.into(), env, cx)?.ok_or_else(|| anyhow!("Invalid face"))?;
    let index = attribute_index(attribute)?;
    let value = validate_attribute(index, value)?;
    let mut targets = Vec::new();
    if frame.is_some_and(|f| f == Object::from(0i64)) {
        targets.push(scoped_vector(face, Some(TRUE), env, cx)?);
        for f in cx.live_frames()? {
            targets.push(scoped_vector(face, Some(f.into()), env, cx)?);
        }
    } else {
        targets.push(scoped_vector(face, frame, env, cx)?);
    }
    for target in &targets {
        checked_vector(*target)?.try_mut()?;
    }
    let generation = env.font_selection.next_generation()?;
    for target in targets {
        checked_vector(target)?.try_mut()?[index].set(value);
    }
    env.font_selection.commit_generation(generation);
    Ok(face)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cyclic_box_metadata_has_bounded_validation() {
        use super::*;
        let roots = crate::core::gc::RootSet::default();
        let cx = &Context::new(&roots);
        let key: Object = crate::lread::intern(":line-width", cx).into();
        let first = Cons::new(key, NIL, cx);
        let second = Cons::new(1_i64, Object::from(first), cx);
        first.set_cdr(second.into()).unwrap();
        assert!(!valid_box(first.into()));
        let mut bounded = NIL;
        for _ in 0..256 {
            bounded = Cons::new(key, Object::from(Cons::new(1_i64, bounded, cx)), cx).into();
        }
        assert!(valid_box(bounded));
        bounded = Cons::new(key, Object::from(Cons::new(1_i64, bounded, cx)), cx).into();
        assert!(!valid_box(bounded));
    }
    #[test]
    fn box_metadata_matches_gnu_and_preserves_input_identity() {
        for value in ["nil", "t", "1", "-1", "'(0 . 0)", "\"red\"", "'(:line-width 1 :style released-button)", "'(:line-width (-1 . 2) :color \"red\" :style flat-button)", "'(:color nil :style nil)", "'(:line-width)"] {
            crate::assert_elprop!("{}", &format!("(progn (internal-make-lisp-face 'rune-box) (let ((v {value})) (internal-set-lisp-face-attribute 'rune-box :box v t) (list (equal (internal-get-lisp-face-attribute 'rune-box :box t) (if (eq v t) 1 v)) (eq (internal-get-lisp-face-attribute 'rune-box :box t) (if (eq v t) 1 v)))))"));
        }
    }
    #[test]
    fn malformed_box_metadata_is_rejected_before_mutation() {
        for value in ["0", "\"\"", "'(:line-width 0)", "'(:line-width (1 . 0))", "'(:style sunken)", "'(:color \"\")", "'(:unknown t)", "'(:line-width . 1)"] {
            crate::assert_elprop!("{}", &format!("(progn (internal-make-lisp-face 'rune-box-invalid) (internal-set-lisp-face-attribute 'rune-box-invalid :box 2 t) (condition-case nil (internal-set-lisp-face-attribute 'rune-box-invalid :box {value} t) (error nil)) (internal-get-lisp-face-attribute 'rune-box-invalid :box t))"));
        }
    }
    #[test]
    fn minibuffer_prompt_policy_has_real_c_default_then_lisp_face_append() {
        use super::*;
        use rune_core::macros::root;
        let roots = crate::core::gc::RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        root!(env, init(Env::new_isolated(cx).unwrap()), cx);
        crate::core::env::init_variables(cx, env);
        crate::lread::load_internal("(if (equal minibuffer-prompt-properties '(read-only t)) nil (error \"wrong C prompt default\")) (setq minibuffer-prompt-properties (append minibuffer-prompt-properties '(face minibuffer-prompt)))", cx, env).unwrap();
        cx.garbage_collect(true);
        crate::lread::load_internal("(if (equal minibuffer-prompt-properties '(read-only t face minibuffer-prompt)) nil (error \"wrong prompt face policy\"))", cx, env).unwrap();
    }
    #[test]
    fn x_resource_policy_has_real_native_false_default_and_remains_settable() {
        use crate::core::gc::RootSet;use rune_core::macros::root;use super::*;
        let roots=RootSet::default();let cx=&mut Context::new(&roots);sym::init_symbols();root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        crate::core::env::init_variables(cx,env);
        assert!(crate::data::symbol_value(sym::INHIBIT_X_RESOURCES,env,cx).unwrap().is_nil());
        crate::lread::load_internal("(setq inhibit-x-resources t)",cx,env).unwrap();
        assert_eq!(crate::data::symbol_value(sym::INHIBIT_X_RESOURCES,env,cx).unwrap(),TRUE);
        crate::lread::load_internal("(setq inhibit-x-resources nil)",cx,env).unwrap();
        assert!(crate::data::symbol_value(sym::INHIBIT_X_RESOURCES,env,cx).unwrap().is_nil());
    }
    #[test]
    fn alias_first_local_creation_preserves_observed_gnu_lookup_quirk() {
        // GNU 30.1 resolves lookup but inserts the original alias key for a
        // first local make. Preserve the surprising reference behavior rather
        // than silently claim a canonical-insertion compatibility improvement.
        crate::assert_elprop!("(progn (internal-make-lisp-face 'rune-quirk-base) (put 'rune-quirk-alias 'face-alias 'rune-quirk-base) (let ((v (internal-make-lisp-face 'rune-quirk-alias (selected-frame)))) (list (eq (internal-make-lisp-face 'rune-quirk-alias) (internal-lisp-face-p 'rune-quirk-base)) (eq v (internal-lisp-face-p 'rune-quirk-base (selected-frame))) (eq v (internal-lisp-face-p 'rune-quirk-alias (selected-frame))))))");
    }
    #[test]
    fn real_face_vector_identity_alias_and_repeat_make_reset_match_gnu() {
        for source in [
            "(let ((v (internal-make-lisp-face 'rune-meta-face))) (list (vectorp v) (length v) (aref v 0) (eq v (internal-lisp-face-p 'rune-meta-face)) (internal-lisp-face-p 42) (internal-lisp-face-p (make-symbol \"rune-meta-face\"))))",
            "(let ((v (internal-make-lisp-face 'rune-reset-face))) (internal-set-lisp-face-attribute 'rune-reset-face :foreground \"red\" t) (list (eq v (internal-make-lisp-face 'rune-reset-face)) (internal-get-lisp-face-attribute 'rune-reset-face :foreground t)))",
            "(progn (internal-make-lisp-face 'rune-target-face) (put 'rune-alias-face 'face-alias 'rune-target-face) (list (eq (internal-lisp-face-p 'rune-alias-face) (internal-lisp-face-p 'rune-target-face)) (internal-set-lisp-face-attribute 'rune-alias-face :weight 'bold t) (internal-get-lisp-face-attribute 'rune-target-face :weight t)))",
        ] {
            crate::assert_elprop!("{}", source);
        }
    }
    #[test]
    fn global_and_frame_local_authorities_and_selector_asymmetry_match_gnu() {
        crate::assert_elprop!(
            "(let* ((f (selected-frame)) (g (internal-make-lisp-face 'rune-scoped-face))) (internal-set-lisp-face-attribute 'rune-scoped-face :foreground \"red\" t) (let ((local (internal-make-lisp-face 'rune-scoped-face f))) (list (eq g local) (internal-get-lisp-face-attribute 'rune-scoped-face :foreground t) (internal-get-lisp-face-attribute 'rune-scoped-face :foreground nil) (progn (internal-set-lisp-face-attribute 'rune-scoped-face :foreground \"blue\" nil) (internal-get-lisp-face-attribute 'rune-scoped-face :foreground f)) (progn (internal-set-lisp-face-attribute 'rune-scoped-face :foreground \"green\" 0) (let ((a (internal-get-lisp-face-attribute 'rune-scoped-face :foreground t)) (b (internal-get-lisp-face-attribute 'rune-scoped-face :foreground f))) (list (equal a \"green\") (eq a b)))))))"
        );
    }
    #[test]
    fn face_metadata_survives_real_collection_and_invalid_update_preserves_value() {
        crate::assert_elprop!(
            "(progn (internal-make-lisp-face 'rune-gc-face (selected-frame)) (internal-set-lisp-face-attribute 'rune-gc-face :inherit '(bold italic) t) (garbage-collect) (list (internal-get-lisp-face-attribute 'rune-gc-face :inherit t) (condition-case nil (internal-set-lisp-face-attribute 'rune-gc-face :weight 42 t) (error 'rejected)) (internal-get-lisp-face-attribute 'rune-gc-face :weight t)))"
        );
    }
}

mod publication;
pub(crate) use publication::{default_face as resolved_default_face, text_face_runs as resolved_text_face_runs};
