//! Opt-in owner-side weight/slant projection. Not wired to frame protocol/paint.
use super::*;
use rune_editor::display::{MeasuredFontCapabilities, MeasuredFontVariant};
use std::ops::Range;
type PResult<T> = std::result::Result<T, &'static str>;
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FontVariantRun {
    pub range: Range<usize>,
    pub variant: MeasuredFontVariant,
}
#[derive(Clone, Copy, Default)]
struct Emphasis {
    bold: Option<bool>,
    italic: Option<bool>,
}
impl Emphasis {
    fn over(self, lower: Self) -> Self {
        Self {
            bold: self.bold.or(lower.bold),
            italic: self.italic.or(lower.italic),
        }
    }
    fn variant(self) -> MeasuredFontVariant {
        match (self.bold.unwrap_or(false), self.italic.unwrap_or(false)) {
            (false, false) => MeasuredFontVariant::Normal,
            (true, false) => MeasuredFontVariant::Bold,
            (false, true) => MeasuredFontVariant::Italic,
            (true, true) => MeasuredFontVariant::BoldItalic,
        }
    }
}
fn attribute(value: Object, enabled: &str) -> PResult<Option<bool>> {
    if value == sym::UNSPECIFIED {
        return Ok(None);
    }
    match value.untag() {
        ObjectType::Symbol(s) if s.interned() && s.name() == "normal" => Ok(Some(false)),
        ObjectType::Symbol(s) if s.interned() && s.name() == enabled => Ok(Some(true)),
        _ => Err("unsupported weight or slant publication value"),
    }
}
struct Resolve<'a, 'env, 'roots> {
    env: &'a Rt<Env<'env>>,
    cx: &'a Context<'roots>,
    work: usize,
    name_bytes: usize,
}
impl Resolve<'_, '_, '_> {
    fn charge(&mut self) -> PResult<()> {
        self.work += 1;
        if self.work > 32768 {
            Err("font resolution work limit exceeded")
        } else {
            Ok(())
        }
    }
    fn name<'a>(&mut self, value: Object<'a>) -> PResult<Symbol<'a>> {
        match value.untag() {
            ObjectType::Symbol(s) => Ok(s),
            ObjectType::String(s) if s.as_bytes().len() <= 65536 => {
                self.name_bytes = self
                    .name_bytes
                    .checked_add(s.as_bytes().len())
                    .ok_or("font face name budget exceeded")?;
                if self.name_bytes > 1024 * 1024 {
                    return Err("font face name budget exceeded");
                }
                {
                    let name = crate::lread::intern_soft(value, None)
                        .map_err(|_| "invalid font face name")?;
                    if name == sym::NIL && s.as_bytes() != b"nil" {
                        return Err("unknown font face string");
                    }
                    Ok(name)
                }
            }
            _ => Err("unsupported or oversized font face name"),
        }
    }
    fn named(&mut self, value: Object, depth: usize, missing_default: bool) -> PResult<Emphasis> {
        let mut name = self.name(value)?;
        let mut seen = Vec::new();
        loop {
            self.charge()?;
            if seen.len() >= 256 || seen.contains(&name) {
                return Err("font face alias cycle or limit exceeded");
            }
            seen.push(name);
            let alias = crate::data::get(name, sym::FACE_ALIAS, self.env, self.cx);
            if alias.is_nil() {
                break;
            }
            name = self.name(alias)?;
        }
        let frame = self
            .cx
            .selected_frame()
            .map_err(|_| "no live owner frame for font publication")?;
        frame
            .validate(self.cx)
            .map_err(|_| "invalid owner frame for font publication")?;
        let Some(table) = self.env.frame_face_tables.get(Object::from(frame)) else {
            return if missing_default && name == sym::DEFAULT && seen.len() == 1 {
                Ok(Emphasis::default())
            } else {
                Err("missing frame face table")
            };
        };
        let ObjectType::HashTable(table) = table.bind(self.cx).untag() else {
            return Err("invalid frame face table");
        };
        let Some(value) = table.get(name.into()) else {
            return if missing_default && name == sym::DEFAULT && seen.len() == 1 {
                Ok(Emphasis::default())
            } else {
                Err("unknown frame-local font face")
            };
        };
        let vector = checked_vector(value).map_err(|_| "invalid font face vector")?;
        if [1, 2, 3, 4, 15, 17]
            .iter()
            .any(|&i| vector[i].get() != sym::UNSPECIFIED)
        {
            return Err("named font geometry or family override publication unsupported");
        }
        let own = Emphasis {
            bold: attribute(vector[5].get(), "bold")?,
            italic: attribute(vector[6].get(), "italic")?,
        };
        Ok(own.over(self.spec(vector[16].get(), depth + 1)?))
    }
    fn spec(&mut self, value: Object, depth: usize) -> PResult<Emphasis> {
        self.charge()?;
        if depth > 32 {
            return Err("font face inheritance depth exceeded");
        }
        if value.is_nil() || value == sym::UNSPECIFIED {
            return Ok(Emphasis::default());
        }
        match value.untag() {
            ObjectType::Symbol(_) | ObjectType::String(_) => self.named(value, depth, false),
            ObjectType::Cons(first) => {
                let anonymous = matches!(first.car().untag(),ObjectType::Symbol(s) if s.interned() && s.name().starts_with(':'));
                let mut result = Emphasis::default();
                let mut tail = value;
                let mut inherit = None;
                let mut keys = Vec::new();
                for _ in 0..256 {
                    if tail.is_nil() {
                        return if let Some(parent) = inherit {
                            Ok(result.over(self.spec(parent, depth + 1)?))
                        } else {
                            Ok(result)
                        };
                    }
                    self.charge()?;
                    let ObjectType::Cons(cell) = tail.untag() else {
                        return Err("improper font face list");
                    };
                    let item = cell.car();
                    tail = cell.cdr();
                    if !anonymous {
                        result = result.over(self.spec(item, depth + 1)?);
                        continue;
                    }
                    let ObjectType::Cons(cell) = tail.untag() else {
                        return Err("odd font face plist");
                    };
                    let v = cell.car();
                    tail = cell.cdr();
                    let ObjectType::Symbol(key) = item.untag() else {
                        return Err("invalid font attribute key");
                    };
                    if !key.interned() || keys.contains(&key) {
                        return Err("duplicate or uninterned font attribute key");
                    }
                    keys.push(key);
                    match key.name() {
                        ":weight" => result.bold = attribute(v, "bold")?,
                        ":slant" => result.italic = attribute(v, "italic")?,
                        ":inherit" => inherit = Some(v),
                        ":foreground"
                        | ":background"
                        | ":underline"
                        | ":strike-through"
                        | ":inverse-video"
                        | ":overline"
                        | ":box"
                        | ":stipple"
                        | ":distant-foreground"
                        | ":extend" => {}
                        // Font-changing attributes outside this measured subset
                        // must not masquerade as compatible baseline requests.
                        ":family" | ":foundry" | ":width" | ":height" | ":font" | ":fontset" => {
                            if v != sym::UNSPECIFIED {
                                return Err(
                                    "font geometry or family override publication unsupported",
                                );
                            }
                        }
                        _ => return Err("unsupported font face attribute"),
                    }
                }
                if tail.is_nil() {
                    if let Some(parent) = inherit {
                        Ok(result.over(self.spec(parent, depth + 1)?))
                    } else {
                        Ok(result)
                    }
                } else {
                    Err("font face list length or cycle exceeded")
                }
            }
            _ => Err("unsupported font face specification"),
        }
    }
    fn property<'a>(&mut self, mut value: Object<'a>) -> PResult<Object<'a>> {
        let mut found = None;
        for _ in 0..256 {
            if value.is_nil() {
                return Ok(found.unwrap_or(NIL));
            }
            self.charge()?;
            let ObjectType::Cons(key) = value.untag() else {
                return Err("improper font property list");
            };
            let ObjectType::Cons(item) = key.cdr().untag() else {
                return Err("odd font property list");
            };
            if matches!(key.car().untag(),ObjectType::Symbol(s) if s.interned() && s.name()=="font-lock-face")
                && !item.car().is_nil()
            {
                return Err("font-lock-face font publication unsupported");
            }
            if key.car() == sym::FACE && found.is_none() {
                found = Some(item.car())
            }
            value = item.cdr();
        }
        if value.is_nil() {
            Ok(found.unwrap_or(NIL))
        } else {
            Err("font property list length or cycle exceeded")
        }
    }
}
pub(crate) fn resolve(
    doc: &crate::editor_document::RuntimeDocument,
    visible: Range<usize>,
    capabilities: MeasuredFontCapabilities,
    env: &Rt<Env>,
    cx: &Context,
) -> PResult<Vec<FontVariantRun>> {
    if !capabilities.matches_geometry(capabilities.bank_id, capabilities.metrics) {
        return Err("invalid measured font capabilities");
    }
    if visible.start > visible.end || visible.end.saturating_sub(visible.start) > 100000 {
        return Err("invalid or excessive font publication range");
    }
    let mut resolver = Resolve {
        env,
        cx,
        work: 0,
        name_bytes: 0,
    };
    let base = resolver.named(sym::DEFAULT.into(), 0, true)?;
    let runs = doc
        .properties()
        .runs_in(rune_text::CharPos(visible.start)..rune_text::CharPos(visible.end))
        .map_err(|_| "invalid font property range")?;
    let mut out: Vec<FontVariantRun> = Vec::new();
    for (i, (range, id)) in runs.enumerate() {
        if i >= 4096 {
            return Err("font property run limit exceeded");
        }
        resolver.charge()?;
        let plist = doc
            .resolve(id.0, cx)
            .map_err(|_| "font property owner resolution failed")?;
        let spec = resolver.property(plist)?;
        let variant = resolver.spec(spec, 0)?.over(base).variant();
        if !capabilities.supports_variant(capabilities.bank_id, capabilities.metrics, variant) {
            return Err("requested font variant unavailable in measured bank");
        }
        let range = range.start.0..range.end.0;
        if let Some(last) = out
            .last_mut()
            .filter(|last| last.variant == variant && last.range.end == range.start)
        {
            last.range.end = range.end;
        } else {
            out.push(FontVariantRun { range, variant })
        }
    }
    Ok(out)
}
