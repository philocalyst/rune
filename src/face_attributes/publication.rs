//! Bounded owner-only buffer face colors and single-line underline publication.
//! No overlay, full font-lock, font variant or structured decoration parity.
use super::*;
use rune_editor::display::{Color, Face};

fn color(value: Object, fallback: Color) -> std::result::Result<Color, &'static str> {
    if value.is_nil() || value == sym::UNSPECIFIED { return Ok(fallback); }
    let value: &str = match value.untag() {
        ObjectType::String(value) => value,
        ObjectType::ByteString(value) if value.len() <= 14 => std::str::from_utf8(value).map_err(|_|"invalid byte face color")?,
        _ => return Err("invalid default face color type"),
    };
    if value == "unspecified-fg" || value == "unspecified-bg" { return Ok(fallback); }
    let named = match value {
        "black" => Some((0,0,0)), "white" => Some((255,255,255)),
        "red" => Some((255,0,0)), "green" => Some((0,255,0)),
        "blue" => Some((0,0,255)), "yellow" => Some((255,255,0)),
        "cyan" => Some((0,255,255)), "magenta" => Some((255,0,255)),
        _ => None,
    };
    if let Some((r,g,b)) = named { return Ok(Color(r,g,b,255)); }
    let Some(hex) = value.strip_prefix('#') else { return Err("unsupported default face color name"); };
    fn component(s: &str) -> Option<u8> { u8::from_str_radix(s,16).ok() }
    // Reject by O(1) length before scanning caller-mutable color bytes.
    if !matches!(hex.len(), 3 | 6) { return Err("unsupported hexadecimal face color length"); }
    // All slicing below follows this bounded byte/ASCII validation.
    if !hex.is_ascii() { return Err("invalid hexadecimal face color"); }
    let rgb = match hex.len() {
        3 => component(&hex[0..1]).zip(component(&hex[1..2])).zip(component(&hex[2..3])).map(|((r,g),b)|(r*17,g*17,b*17)),
        6 => component(&hex[0..2]).zip(component(&hex[2..4])).zip(component(&hex[4..6])).map(|((r,g),b)|(r,g,b)),
        _ => None,
    };
    rgb.map(|(r,g,b)|Color(r,g,b,255)).ok_or("unsupported or invalid hexadecimal face color")
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Underline { Off, Foreground, Color(Color) }
impl Underline {
    fn resolve(self,foreground:Color)->Option<Color>{match self{Self::Off=>None,Self::Foreground=>Some(foreground),Self::Color(color)=>Some(color)}}
}
fn underline(value:Object,fallback:Color)->std::result::Result<Option<Underline>,&'static str>{
    if value==sym::UNSPECIFIED{return Ok(None);}
    if value.is_nil(){return Ok(Some(Underline::Off));}
    if value==TRUE{return Ok(Some(Underline::Foreground));}
    match value.untag(){
        ObjectType::String(_)|ObjectType::ByteString(_)=>color(value,fallback).map(|c|Some(Underline::Color(c))),
        _=>Err("structured or invalid underline publication unsupported"),
    }
}
/// Read only rooted tables; no lazy table creation, Lisp evaluation, or GC.
/// Recompute every owner turn so mutable vectors/strings cannot evade revision.
fn default_face_data(env: &Rt<Env>, cx: &Context) -> std::result::Result<(Face,Underline), &'static str> {
    let frame = cx.selected_frame().map_err(|_|"no live owner frame for face publication")?;
    frame.validate(cx).map_err(|_|"invalid owner frame for face publication")?;
    let fallback = Face::default();
    let Some(table) = env.frame_face_tables.get(Object::from(frame)) else { return Ok((fallback,Underline::Off)); };
    let table: &LispHashTable = table.bind(cx).try_into().map_err(|_|"invalid frame face table")?;
    let mut name = sym::DEFAULT;
    let mut seen = Vec::new();
    for _ in 0..256 {
        if seen.contains(&name) { return Err("cyclic default face alias"); }
        seen.push(name);
        let alias = crate::data::get(name, sym::FACE_ALIAS, env, cx);
        if alias.is_nil() { break; }
        let ObjectType::Symbol(target) = alias.untag() else { return Err("unsupported default face alias type"); };
        name = target;
    }
    if seen.len() == 256 { return Err("default face alias limit exceeded"); }
    let Some(value) = table.get(name.into()) else { return Ok((fallback,Underline::Off)); };
    let vector = checked_vector(value).map_err(|_|"invalid default face vector")?;
    let inverse=vector[8].get();
    if !inverse.is_nil() && inverse!=sym::UNSPECIFIED {return Err("inverse-video default face publication unsupported");}
    let inherited=vector[16].get();
    if !inherited.is_nil() && inherited!=sym::UNSPECIFIED {return Err("default face inheritance publication unsupported");}
    let foreground=color(vector[9].get(),fallback.foreground)?;
    let line=underline(vector[7].get(),fallback.foreground)?.unwrap_or(Underline::Off);
    Ok((Face { id:0,foreground,background:color(vector[10].get(),fallback.background)?,underline:line.resolve(foreground) },line))
}
pub(crate) fn default_face(env:&Rt<Env>,cx:&Context)->std::result::Result<Face,&'static str>{default_face_data(env,cx).map(|(face,_)|face)}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_editor::{Document, EditorHost};
    #[test]
    fn oversized_mutable_color_fails_bounded_format_validation() {
        let roots=crate::core::gc::RootSet::default();
        let cx=&Context::new(&roots);
        let value=cx.add(format!("#{}", "a".repeat(1024*1024)));
        assert_eq!(color(value,Face::default().foreground),Err("unsupported hexadecimal face color length"));
    }
    #[test]
    fn default_colors_publish_plain_spans_and_detect_mutable_bypasses() {
        with_lisp_runtime_owner(80,24,|mut host| {
            host.evaluate_source("(insert \"λ🙂\") (internal-make-lisp-face 'default (selected-frame)) (internal-set-lisp-face-attribute 'default :foreground \"#123456\") (internal-set-lisp-face-attribute 'default :background \"#abc\")").map_err(anyhow::Error::msg)?;
            let snap = host.snapshot()?;
            let view = snap.views.iter().find(|v|v.id==snap.selected_window).unwrap();
            let buffer = snap.buffers.iter().find(|b|b.id==view.view.buffer).unwrap();
            let (window,id,len,sequence)=(view.id,buffer.id,buffer.document.len_chars(),snap.sequence);
            drop(snap);
            let revision = host.display_style_revision()?;
            let style = host.display_window_style(window,id,len)?;
            assert_eq!(style.default_face.foreground,Color(0x12,0x34,0x56,255));
            assert_eq!(style.default_face.background,Color(0xaa,0xbb,0xcc,255));
            assert_eq!(style.spans[0].range,0..2);
            assert_eq!(style.spans[0].face,style.default_face);
            host.evaluate_source("(aset (internal-lisp-face-p 'default (selected-frame)) 9 \"#654321\") (garbage-collect)").map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.sequence,sequence);
            let bypass = host.display_style_revision()?;
            assert_eq!(revision.epoch,bypass.epoch,"aset bypasses setter generation");
            assert_ne!(revision,bypass,"plain colors independently detect mutation");
            assert_eq!(style.default_face.foreground,Color(0x12,0x34,0x56,255),"old DTO stays immutable after Lisp GC");
            host.evaluate_source("(setq native-color (copy-sequence \"#abcdef\")) (internal-set-lisp-face-attribute 'default :foreground native-color) (clear-string native-color)").map_err(anyhow::Error::msg)?;
            assert!(host.display_style_revision()?.default_face.is_err());
            assert!(host.display_window_style(window,id,len).is_err());
            host.evaluate_source("(internal-set-lisp-face-attribute 'default :foreground \"red\")").map_err(anyhow::Error::msg)?;
            assert_eq!(host.display_window_style(window,id,len)?.default_face.foreground,Color(255,0,0,255));
            assert!(host.display_window_style(window,id,len+1).is_err());
            Ok(())
        }).unwrap();
    }
}
#[cfg(all(test, feature="native"))]
mod joined_tests {
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_editor::EditorHost;
    use rune_gpui::{editor_bridge::EditorBridge,editor_transport::{OwnerEvent,pending_editor_transport}};
    use rune_display::{Color,FixedMetrics,Rect,host_updates};
    use std::{future::Future,task::{Context,Poll,Waker}};
    fn next_frame(updates: &mut rune_display::HostUpdates) -> std::sync::Arc<rune_display::FrameDisplay> {
        let mut next=std::pin::pin!(updates.next());
        let mut cx=Context::from_waker(Waker::noop());
        match next.as_mut().poll(&mut cx) { Poll::Ready(Some(u))=>u.frame.expect("frame"), _=>panic!("missing frame") }
    }
    #[test]
    fn fifo_face_only_changes_publish_headless_colors_even_after_signal_and_aset() {
        with_lisp_runtime_owner(80,24,|mut host| {
            host.evaluate_source("(insert \"λ🙂\") (internal-make-lisp-face 'default (selected-frame))").map_err(anyhow::Error::msg)?;
            let (publisher,mut updates)=host_updates();
            let (sender,_reply,pending)=pending_editor_transport(4,publisher);
            let bridge=EditorBridge::new(host,FixedMetrics::default(),Rect{x:0.,y:0.,width:640.,height:480.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
            let (platform,reader)=rune_gpui::platform_context::platform_frames();
            let mut worker=pending.bind_ime_initialized(bridge,platform).map_err(|e|anyhow::anyhow!("{e:?}"))?.with_evaluator(|h,s|h.evaluate_source(s));
            let initial_identity=reader.latest().unwrap().input.as_ref().unwrap().identity();
            let first=next_frame(&mut updates);
            let sequence=worker.bridge().snapshot().unwrap().sequence;
            sender.try_send(OwnerEvent::Evaluate{request_id:901,source:"(internal-set-lisp-face-attribute 'default :foreground \"#123456\") (internal-set-lisp-face-attribute 'default :background \"#abcdef\") (error \"after face edit\")".into()}).unwrap();
            let step=worker.try_step().unwrap();
            assert!(step.evaluation.unwrap().outcome.is_err());
            assert_eq!(step.edit.unwrap(),true);
            assert!(step.render_error.is_none());
            assert_eq!(worker.bridge().snapshot().unwrap().sequence,sequence,"style not input authority");
            let frame=next_frame(&mut updates);
            assert!(frame.generation>first.generation);
            assert_eq!(frame.windows[0].background,Color(0xab,0xcd,0xef,255));
            assert_eq!(frame.windows[0].rows[0].clusters[0].face.foreground,Color(0x12,0x34,0x56,255));
            let mut target=rune_display::svg::SvgTarget::default();
            rune_display::paint_frame(&frame,FixedMetrics::default().0,&mut target);
            let svg=target.finish(frame.bounds);
            assert!(svg.contains("fill=\"#123456\""));
            assert!(svg.contains("fill=\"#abcdef\""));
            sender.try_send(OwnerEvent::Evaluate{request_id:902,source:"(aset (internal-lisp-face-p 'default (selected-frame)) 9 \"#654321\")".into()}).unwrap();
            assert_eq!(worker.try_step().unwrap().edit.unwrap(),true);
            assert_eq!(next_frame(&mut updates).windows[0].rows[0].clusters[0].face.foreground,Color(0x65,0x43,0x21,255));
            sender.try_send(OwnerEvent::Evaluate{request_id:903,source:"(aset (internal-lisp-face-p 'default (selected-frame)) 9 \"not-a-supported-color\")".into()}).unwrap();
            let step=worker.try_step().unwrap();
            assert!(step.evaluation.unwrap().outcome.is_ok());
            assert!(step.render_error.is_some());
            assert_eq!(reader.latest().unwrap().input.as_ref().unwrap().identity(),initial_identity,"style errors must not invalidate unchanged input authority");
            sender.try_send(OwnerEvent::Evaluate{request_id:904,source:"(internal-set-lisp-face-attribute 'default :foreground \"blue\")".into()}).unwrap();
            let step=worker.try_step().unwrap();
            assert!(step.evaluation.unwrap().outcome.is_ok(),"invalid prior style must not block repair");
            assert!(step.render_error.is_none());
            assert_eq!(next_frame(&mut updates).windows[0].rows[0].clusters[0].face.foreground,Color(0,0,255,255));
            Ok(())
        }).unwrap();
    }
}

#[derive(Clone,Copy,Default)]
struct Colors { fg:Option<Color>, bg:Option<Color>, underline:Option<Underline> }
impl Colors {
    fn over(self, lower:Self)->Self {Self{fg:self.fg.or(lower.fg),bg:self.bg.or(lower.bg),underline:self.underline.or(lower.underline)}}
    fn face(self, base:Face,base_underline:Underline)->Face {
        let foreground=self.fg.unwrap_or(base.foreground);
        Face{foreground,background:self.bg.unwrap_or(base.background),underline:self.underline.unwrap_or(base_underline).resolve(foreground),..base}
    }
}
struct Resolve<'a,'env,'roots> { env:&'a Rt<Env<'env>>, cx:&'a Context<'roots>, base:Face, work:usize, name_bytes:usize }
impl Resolve<'_,'_,'_> {
    fn charge(&mut self)->std::result::Result<(),&'static str> {self.work+=1;if self.work>32768 {Err("face run resolution work limit exceeded")}else{Ok(())}}
    fn name<'a>(&mut self,value:Object<'a>)->std::result::Result<Symbol<'a>,&'static str> {
        match value.untag() {
            ObjectType::Symbol(s)=>Ok(s),
            ObjectType::String(s) if s.as_bytes().len()<=65536=>{
                self.name_bytes=self.name_bytes.checked_add(s.as_bytes().len()).ok_or("face name byte budget exceeded")?;
                if self.name_bytes>1024*1024{return Err("face name byte budget exceeded");}
                crate::lread::intern_soft(value,None).map_err(|_|"invalid face name")
            },
            _=>Err("unsupported or oversized face name"),
        }
    }
    fn optional_color(&self,v:Object,fallback:Color)->std::result::Result<Option<Color>,&'static str> {
        if v.is_nil()||v==sym::UNSPECIFIED {Ok(None)} else {color(v,fallback).map(Some)}
    }
    fn spec(&mut self,value:Object,depth:usize)->std::result::Result<Colors,&'static str> {
        self.charge()?;
        if depth>32 {return Err("face inheritance depth exceeded");}
        if value.is_nil() || value==sym::UNSPECIFIED {return Ok(Colors::default());}
        match value.untag() {
            ObjectType::Symbol(_)|ObjectType::String(_)=>{
                let mut name=self.name(value)?;
                let mut seen=Vec::new();
                loop {
                    self.charge()?;
                    if seen.len()>=256||seen.contains(&name) {return Err("face alias cycle or limit exceeded");}
                    seen.push(name);
                    let alias=crate::data::get(name,sym::FACE_ALIAS,self.env,self.cx);
                    if alias.is_nil(){break;}
                    name=self.name(alias)?;
                }
                let frame=self.cx.selected_frame().map_err(|_|"no live frame for face runs")?;
                frame.validate(self.cx).map_err(|_|"invalid frame for face runs")?;
                let table=self.env.frame_face_tables.get(Object::from(frame)).ok_or("missing frame face table")?;
                let table:&LispHashTable=table.bind(self.cx).try_into().map_err(|_|"invalid frame face table")?;
                let vector=checked_vector(table.get(name.into()).ok_or("unknown frame-local face")?).map_err(|_|"invalid named face vector")?;
                let inverse=vector[8].get();
                if !inverse.is_nil() && inverse!=sym::UNSPECIFIED {return Err("inverse-video named face publication unsupported");}
                let own=Colors{fg:self.optional_color(vector[9].get(),self.base.foreground)?,bg:self.optional_color(vector[10].get(),self.base.background)?,underline:underline(vector[7].get(),self.base.foreground)?};
                let inherited=self.spec(vector[16].get(),depth+1)?;
                Ok(own.over(inherited))
            }
            ObjectType::Cons(first)=>{
                let anonymous=matches!(first.car().untag(),ObjectType::Symbol(s) if s.interned() && s.name().starts_with(':'));
                let mut result=Colors::default();let mut tail=value;
                let mut inheritance=None;
                let mut keys=Vec::new();
                for _ in 0..256 {
                    if tail.is_nil(){return if let Some(parent)=inheritance {Ok(result.over(self.spec(parent,depth+1)?))}else{Ok(result)};}
                    self.charge()?;
                    let ObjectType::Cons(cell)=tail.untag() else{return Err("improper face list");};
                    let item=cell.car();tail=cell.cdr();
                    if !anonymous {result=result.over(self.spec(item,depth+1)?);continue;}
                    let ObjectType::Cons(cell)=tail.untag() else{return Err("odd face attribute plist");};
                    let v=cell.car();tail=cell.cdr();
                    let ObjectType::Symbol(key)=item.untag() else{return Err("invalid face attribute key");};
                    if !key.interned(){return Err("uninterned face attribute key");}
                    if keys.contains(&key){return Err("duplicate anonymous face attribute unsupported");}
                    keys.push(key);
                    match key.name(){
                        ":foreground"=>if result.fg.is_none(){result.fg=self.optional_color(v,self.base.foreground)?;},
                        ":background"=>if result.bg.is_none(){result.bg=self.optional_color(v,self.base.background)?;},
                        ":inherit"=>if inheritance.is_none(){inheritance=Some(v);},
                        ":underline"=>result.underline=underline(v,self.base.foreground)?,
                        ":inverse-video"=>if !v.is_nil()&&v!=sym::UNSPECIFIED{return Err("inverse-video anonymous face publication unsupported");},
                        ":family"|":foundry"|":width"|":height"|":weight"|":slant"|":overline"|":strike-through"|":box"|":stipple"|":font"|":fontset"|":distant-foreground"|":extend"=>{},
                        _=>return Err("unsupported face attribute key"),
                    }
                }
                if tail.is_nil() {
                    if let Some(parent)=inheritance {Ok(result.over(self.spec(parent,depth+1)?))}else{Ok(result)}
                }else{Err("face list length or cycle limit exceeded")}
            }
            _=>Err("unsupported face specification"),
        }
    }
    fn face_property<'a>(&mut self,mut plist:Object<'a>)->std::result::Result<Object<'a>,&'static str>{
        let mut found=None;
        for _ in 0..256 {
            if plist.is_nil(){return Ok(found.unwrap_or(NIL));}
            self.charge()?;
            let ObjectType::Cons(k)=plist.untag() else{return Err("improper retained property list");};
            let key=k.car();plist=k.cdr();
            let ObjectType::Cons(v)=plist.untag() else{return Err("odd retained property list");};
            plist=v.cdr();
            if matches!(key.untag(),ObjectType::Symbol(k) if k.interned()&&k.name()=="font-lock-face") && !v.car().is_nil(){return Err("font-lock-face property publication unsupported");}
            if key==sym::FACE && found.is_none(){found=Some(v.car());}
        }
        if plist.is_nil(){Ok(found.unwrap_or(NIL))}else{Err("property list length or cycle limit exceeded")}
    }
}
pub(crate) fn text_face_runs(doc:&crate::editor_document::RuntimeDocument,visible:std::ops::Range<usize>,env:&Rt<Env>,cx:&Context)->std::result::Result<Vec<rune_editor::display::FaceSpan>,&'static str>{
    let (base,base_underline)=default_face_data(env,cx)?;
    let mut resolver=Resolve{env,cx,base,work:0,name_bytes:0};
    if visible.end.saturating_sub(visible.start)>100_000{return Err("visible style range limit exceeded");}
    let mut out:Vec<rune_editor::display::FaceSpan>=Vec::new();
    let runs=doc.properties().runs_in(rune_text::CharPos(visible.start)..rune_text::CharPos(visible.end)).map_err(|_|"invalid visible property range")?;
    let mut candidates=0;
    for (range,id) in runs {
        candidates+=1;
        if candidates>4096{return Err("visible face run limit exceeded");}
        resolver.charge()?;
        let plist=doc.resolve(id.0,cx).map_err(|_|"retained property owner resolution failed")?;
        let spec=resolver.face_property(plist)?;
        let face=resolver.spec(spec,0)?.face(base,base_underline);
        let range=range.start.0..range.end.0;
        if let Some(last)=out.last_mut().filter(|last|last.face==face&&last.range.end==range.start){last.range.end=range.end;}
        else{out.push(rune_editor::display::FaceSpan{range,face});}
    }
    Ok(out)
}
#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_editor::{EditorHost,Document};
    fn capture(host:&crate::editor_document::LispRuntimeHost<'_,'_,'_>)->(rune_editor::WindowId,rune_editor::BufferId,usize,u64){
        let s=host.snapshot().unwrap();let v=s.views.iter().find(|v|v.id==s.selected_window).unwrap();
        let b=s.buffers.iter().find(|b|b.id==v.view.buffer).unwrap();(v.id,b.id,b.document.len_chars(),s.sequence)
    }
    #[test]
    fn unicode_buffer_named_anonymous_and_inherited_faces_resolve_to_plain_runs(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"* Heading🙂\\n body λ\\n\") (internal-make-lisp-face 'default (selected-frame)) (internal-make-lisp-face 'rune-parent (selected-frame)) (internal-set-lisp-face-attribute 'rune-parent :foreground \"#123456\") (internal-set-lisp-face-attribute 'rune-parent :background \"#332211\") (internal-make-lisp-face 'rune-child (selected-frame)) (internal-set-lisp-face-attribute 'rune-child :inherit 'rune-parent) (put 'rune-alias 'face-alias 'rune-child) (put-text-property 1 11 'face 'rune-alias) (put-text-property 12 16 'face '(:foreground \"blue\" :background \"#abc\"))").map_err(anyhow::Error::msg)?;
            let(w,b,len,_)=capture(&host);
            let runs=host.display_face_runs(w,b,len,0..len)?.unwrap();
            assert_eq!(runs[0].range,0..10);
            assert_eq!(runs[0].face.foreground,Color(0x12,0x34,0x56,255));
            assert_eq!(runs[0].face.background,Color(0x33,0x22,0x11,255));
            assert!(runs.iter().any(|r|r.range== (11..15)&&r.face.foreground==Color(0,0,255,255)&&r.face.background==Color(0xaa,0xbb,0xcc,255)));
            let old=runs.clone();
            host.evaluate_source("(garbage-collect) (aset (internal-lisp-face-p 'rune-parent (selected-frame)) 9 \"red\")").map_err(anyhow::Error::msg)?;
            assert_eq!(old[0].face.foreground,Color(0x12,0x34,0x56,255));
            assert_eq!(host.display_face_runs(w,b,len,0..10)?.unwrap()[0].face.foreground,Color(255,0,0,255));
            Ok(())
        }).unwrap();
    }
    #[test]
    fn mutable_face_specs_and_cycles_are_bounded_without_stale_plain_results(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"abc\") (setq run-style (list :foreground \"#010203\")) (put-text-property 1 4 'face run-style)").map_err(anyhow::Error::msg)?;
            let(w,b,len,sequence)=capture(&host);
            let old=host.display_face_runs(w,b,len,0..len)?.unwrap();
            host.evaluate_source("(setcar (cdr run-style) \"#040506\")").map_err(anyhow::Error::msg)?;
            assert_eq!(host.snapshot()?.sequence,sequence);
            let new=host.display_face_runs(w,b,len,0..len)?.unwrap();
            assert_ne!(old,new);
            assert_eq!(new[0].face.foreground,Color(4,5,6,255));
            host.evaluate_source("(setq run-cycle (list 'rune-cycle)) (setcdr run-cycle run-cycle) (put-text-property 1 4 'face run-cycle)").map_err(anyhow::Error::msg)?;
            assert!(host.display_face_runs(w,b,len,0..len).is_err());
            host.evaluate_source("(put-text-property 1 4 'face '(:foreground \"green\"))").map_err(anyhow::Error::msg)?;
            assert_eq!(host.display_face_runs(w,b,len,0..len)?.unwrap()[0].face.foreground,Color(0,255,0,255));
            Ok(())
        }).unwrap();
    }
    #[test]
    fn unicode_name_byte_limits_precede_lookup_and_inverse_is_not_silently_ignored(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"a\") (put-text-property 1 2 'face (make-string 32769 ?é))").map_err(anyhow::Error::msg)?;
            let(w,b,len,_)=capture(&host);
            assert!(host.display_face_runs(w,b,len,0..len).unwrap_err().to_string().contains("oversized face name"));
            host.evaluate_source("(put-text-property 1 2 'face '(:foreground \"red\" :inverse-video t))").map_err(anyhow::Error::msg)?;
            assert!(host.display_face_runs(w,b,len,0..len).unwrap_err().to_string().contains("inverse-video"));
            host.evaluate_source("(put-text-property 1 2 'face nil) (put-text-property 1 2 'font-lock-face 'bold)").map_err(anyhow::Error::msg)?;
            assert!(host.display_face_runs(w,b,len,0..len).unwrap_err().to_string().contains("font-lock-face"));
            Ok(())
        }).unwrap();
        use rune_core::macros::root;
        let roots=crate::core::gc::RootSet::default();let cx=&Context::new(&roots);sym::init_symbols();
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        let value=cx.add("x".repeat(65536));
        let mut resolver=Resolve{env,cx,base:Face::default(),work:0,name_bytes:0};
        for _ in 0..16 {resolver.name(value).unwrap();}
        assert_eq!(resolver.name(value).unwrap_err(),"face name byte budget exceeded");
    }
    #[test]
    fn face_list_spine_limit_is_inclusive_and_cycles_still_fail(){
        use rune_core::macros::root;
        let roots=crate::core::gc::RootSet::default();let cx=&Context::new(&roots);sym::init_symbols();
        root!(env,init(Env::new_isolated(cx).unwrap()),cx);
        let mut value=NIL;
        for _ in 0..256 {value=Cons::new(NIL,value,cx).into();}
        let mut resolver=Resolve{env,cx,base:Face::default(),work:0,name_bytes:0};
        assert!(resolver.spec(value,0).is_ok());
        value=Cons::new(NIL,value,cx).into();
        assert!(resolver.spec(value,0).is_err());
    }
    #[test]
    fn visible_ranges_are_indexed_clipped_and_authenticated(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.execute(rune_editor::Command::Insert("x".repeat(200_000)))?;
            host.evaluate_source("(put-text-property 100001 100011 'face '(:foreground \"red\"))").map_err(anyhow::Error::msg)?;
            let(w,b,len,_)=capture(&host);
            let runs=host.display_face_runs(w,b,len,100_003..100_007)?.unwrap();
            assert_eq!(runs.len(),1);assert_eq!(runs[0].range,100_003..100_007);
            assert_eq!(runs[0].face.foreground,Color(255,0,0,255));
            assert!(host.display_face_runs(w,b,len,0..100_001).is_err());
            assert!(host.display_face_runs(w,b,len-1,0..3).is_err());
            Ok(())
        }).unwrap();
    }
}
#[cfg(all(test,feature="native"))]
mod joined_run_tests {
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_editor::{EditorHost,Command};
    use rune_gpui::{editor_bridge::EditorBridge,editor_transport::{OwnerEvent,pending_editor_transport}};
    use rune_display::{Color,FixedMetrics,Rect,host_updates};
    use std::{future::Future,task::{Context,Poll,Waker}};
    fn frame(updates:&mut rune_display::HostUpdates)->std::sync::Arc<rune_display::FrameDisplay>{
        let mut future=std::pin::pin!(updates.next());let mut cx=Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx){Poll::Ready(Some(u))=>u.frame.unwrap(),_=>panic!("missing style frame")}
    }
    #[test]
    fn visible_named_and_mutable_property_colors_publish_through_real_fifo(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"* Heading🙂\\n body λ\\n\") (internal-make-lisp-face 'rune-heading (selected-frame)) (internal-set-lisp-face-attribute 'rune-heading :foreground \"red\") (put-text-property 1 11 'face 'rune-heading)").map_err(anyhow::Error::msg)?;
            host.execute(Command::GoTo(0))?;
            let(publisher,mut updates)=host_updates();let(sender,_replies,pending)=pending_editor_transport(4,publisher);
            let bridge=EditorBridge::new(host,FixedMetrics::default(),Rect{x:0.,y:0.,width:640.,height:480.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
            let(platform,reader)=rune_gpui::platform_context::platform_frames();
            let mut worker=pending.bind_ime_initialized(bridge,platform).map_err(|e|anyhow::anyhow!("{e:?}"))?.with_evaluator(|h,s|h.evaluate_source(s));
            let first=frame(&mut updates);assert_eq!(first.windows[0].rows[0].clusters[0].face.foreground,Color(255,0,0,255));
            let before=worker.bridge().publication_revision().unwrap();let identity=reader.latest().unwrap().input.as_ref().unwrap().identity();
            sender.try_send(OwnerEvent::Evaluate{request_id:951,source:"(aset (internal-lisp-face-p 'rune-heading (selected-frame)) 9 \"blue\")".into()}).unwrap();
            let step=worker.try_step().unwrap();assert_eq!(step.edit.unwrap(),true);assert!(step.render_error.is_none());
            let after=worker.bridge().publication_revision().unwrap();
            assert_eq!(before.0,after.0);assert_eq!(before.1,after.1,"named vector bypass leaves default/setter revision unchanged");assert_ne!(before.2,after.2);
            assert_eq!(reader.latest().unwrap().input.as_ref().unwrap().identity(),identity);
            let painted=frame(&mut updates);assert_eq!(painted.windows[0].rows[0].clusters[0].face.foreground,Color(0,0,255,255));
            sender.try_send(OwnerEvent::Evaluate{request_id:952,source:"(setq mutable-run-face (list :foreground \"red\" :background \"#abc\")) (put-text-property 1 11 'face mutable-run-face)".into()}).unwrap();
            assert!(worker.try_step().unwrap().render_error.is_none());let _=frame(&mut updates);
            let sequence=worker.bridge().snapshot().unwrap().sequence;
            sender.try_send(OwnerEvent::Evaluate{request_id:953,source:"(setcar (cdr mutable-run-face) \"#112233\") (error \"after mutable face edit\")".into()}).unwrap();
            let step=worker.try_step().unwrap();assert!(step.evaluation.unwrap().outcome.is_err());assert_eq!(step.edit.unwrap(),true);assert!(step.render_error.is_none());
            assert_eq!(worker.bridge().snapshot().unwrap().sequence,sequence);
            let styled=frame(&mut updates);assert_eq!(styled.windows[0].rows[0].clusters[0].face.foreground,Color(0x11,0x22,0x33,255));
            let mut target=rune_display::svg::SvgTarget::default();rune_display::paint_frame(&styled,FixedMetrics::default().0,&mut target);let svg=target.finish(styled.bounds);
            assert!(svg.contains("fill=\"#112233\""));assert!(svg.contains("fill=\"#aabbcc\""));
            Ok(())
        }).unwrap();
    }
    #[test]
    fn long_org_scroll_resolves_only_visible_property_slice(){
        with_lisp_runtime_owner(80,24,|mut host|{
            let source=(0..2000).map(|n|format!("* Heading {n} λ🙂\n")).collect::<String>();let len=source.chars().count();
            host.execute(Command::Insert(source))?;host.execute(Command::GoTo(0))?;
            host.evaluate_source(&format!("(put-text-property 1 {} 'face '(:foreground \\\"#123456\\\"))",len+1).replace("\\\"","\"")).map_err(anyhow::Error::msg)?;
            let(publisher,mut updates)=host_updates();let(sender,_replies,pending)=pending_editor_transport(4,publisher);
            let bridge=EditorBridge::new(host,FixedMetrics::default(),Rect{x:0.,y:0.,width:640.,height:480.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
            let mut worker=pending.bind_initialized(bridge).map_err(|e|anyhow::anyhow!("{e:?}"))?;let _=frame(&mut updates);
            let snap=worker.bridge().snapshot().unwrap();let view=snap.views.iter().find(|v|v.id==snap.selected_window).unwrap();
            sender.try_send(OwnerEvent::ScrollWindow{window_id:view.id.as_u64(),buffer_id:view.view.buffer.as_u64(),expected_sequence:snap.sequence,lines:1000}).unwrap();
            assert_eq!(worker.try_step().unwrap().edit.unwrap(),true);let styled=frame(&mut updates);
            let window=&styled.windows[0];assert!(window.rows[0].source.start>1000);assert!(window.scanned_chars<len/4);assert_eq!(window.rows[0].clusters[0].face.foreground,Color(0x12,0x34,0x56,255));
            let revision=worker.bridge().publication_revision().unwrap();assert!(revision.2[0].range.start>1000);assert!(revision.2[0].range.end-revision.2[0].range.start<len/4);
            Ok(())
        }).unwrap();
    }
}

#[cfg(test)]
mod underline_tests {
    use super::*;
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_editor::{EditorHost,Document};
    fn line(host:&crate::editor_document::LispRuntimeHost<'_,'_,'_>)->rune_editor::Result<Face>{
        let s=host.snapshot()?;let v=s.views.iter().find(|v|v.id==s.selected_window).unwrap();let b=s.buffers.iter().find(|b|b.id==v.view.buffer).unwrap();
        Ok(host.display_face_runs(v.id,b.id,b.document.len_chars(),0..b.document.len_chars())?.unwrap()[0].face)
    }
    #[test]
    fn underline_inheritance_distinguishes_off_and_resolves_final_foreground(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"aλ🙂\") (internal-make-lisp-face 'default (selected-frame)) (internal-set-lisp-face-attribute 'default :foreground \"green\") (internal-set-lisp-face-attribute 'default :underline t) (internal-make-lisp-face 'rune-link-parent (selected-frame)) (internal-set-lisp-face-attribute 'rune-link-parent :foreground \"red\") (internal-set-lisp-face-attribute 'rune-link-parent :underline t) (internal-make-lisp-face 'rune-link (selected-frame)) (internal-set-lisp-face-attribute 'rune-link :inherit 'rune-link-parent) (internal-set-lisp-face-attribute 'rune-link :foreground \"blue\") (put-text-property 1 4 'face 'rune-link)").map_err(anyhow::Error::msg)?;
            assert_eq!(line(&host)?.underline,Some(Color(0,0,255,255)));
            host.evaluate_source("(internal-set-lisp-face-attribute 'rune-link :underline nil)").map_err(anyhow::Error::msg)?;assert_eq!(line(&host)?.underline,None);
            host.evaluate_source("(put-text-property 1 4 'face '(:foreground \"blue\" :underline \"red\"))").map_err(anyhow::Error::msg)?;assert_eq!(line(&host)?.underline,Some(Color(255,0,0,255)));
            host.evaluate_source("(put-text-property 1 4 'face '(:foreground \"blue\" :underline nil))").map_err(anyhow::Error::msg)?;assert_eq!(line(&host)?.underline,None);
            host.evaluate_source("(put-text-property 1 4 'face '(:foreground \"blue\" :underline unspecified))").map_err(anyhow::Error::msg)?;assert_eq!(line(&host)?.underline,Some(Color(0,0,255,255)),"default t resolves after named foreground override");
            host.evaluate_source("(put-text-property 1 4 'face '(:underline (:style wave)))").map_err(anyhow::Error::msg)?;assert!(line(&host).is_err());
            Ok(())
        }).unwrap();
    }
    #[test]
    fn mutable_underline_color_is_recomputed_and_old_dto_survives_gc(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"a\") (setq rune-line-color (copy-sequence \"#123456\")) (setq rune-line-spec (list :underline rune-line-color)) (put-text-property 1 2 'face rune-line-spec)").map_err(anyhow::Error::msg)?;
            let before=line(&host)?;let sequence=host.snapshot()?.sequence;
            host.evaluate_source("(clear-string rune-line-color) (garbage-collect)").map_err(anyhow::Error::msg)?;
            assert_eq!(before.underline,Some(Color(0x12,0x34,0x56,255)));assert_eq!(host.snapshot()?.sequence,sequence);assert!(line(&host).is_err());
            host.evaluate_source("(setcar (cdr rune-line-spec) t)").map_err(anyhow::Error::msg)?;assert_eq!(line(&host)?.underline,Some(line(&host)?.foreground));
            Ok(())
        }).unwrap();
    }
}
#[cfg(all(test,feature="native"))]
mod joined_underline_tests {
    use crate::editor_document::with_lisp_runtime_owner;
    use rune_editor::EditorHost;
    use rune_gpui::{editor_bridge::EditorBridge,editor_transport::{OwnerEvent,pending_editor_transport}};
    use rune_display::{Color,FixedMetrics,Rect,host_updates};
    use std::{future::Future,task::{Context,Poll,Waker}};
    fn frame(updates:&mut rune_display::HostUpdates)->std::sync::Arc<rune_display::FrameDisplay>{
        let mut future=std::pin::pin!(updates.next());let mut cx=Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx){Poll::Ready(Some(u))=>u.frame.unwrap(),_=>panic!("missing underline frame")}
    }
    #[test]
    fn org_link_underline_toggles_publish_without_text_geometry_or_ime_changes(){
        with_lisp_runtime_owner(80,24,|mut host|{
            host.evaluate_source("(insert \"[link]🙂\") (internal-make-lisp-face 'rune-org-link (selected-frame)) (internal-set-lisp-face-attribute 'rune-org-link :foreground \"blue\") (internal-set-lisp-face-attribute 'rune-org-link :underline t) (put-text-property 1 8 'face 'rune-org-link)").map_err(anyhow::Error::msg)?;
            let(publisher,mut updates)=host_updates();let(sender,_replies,pending)=pending_editor_transport(4,publisher);
            let bridge=EditorBridge::new(host,FixedMetrics::default(),Rect{x:0.,y:0.,width:640.,height:480.}).map_err(|e|anyhow::anyhow!("{e:?}"))?;
            let(platform,reader)=rune_gpui::platform_context::platform_frames();
            let mut worker=pending.bind_ime_initialized(bridge,platform).map_err(|e|anyhow::anyhow!("{e:?}"))?.with_evaluator(|h,s|h.evaluate_source(s));
            let first=frame(&mut updates);assert_eq!(first.protocol_version,2);
            assert_eq!(first.windows[0].rows[0].clusters[0].face.underline,Some(Color(0,0,255,255)));
            let identity=reader.latest().unwrap().input.as_ref().unwrap().identity();let sequence=worker.bridge().snapshot().unwrap().sequence;
            let mut svg=rune_display::svg::SvgTarget::default();rune_display::paint_frame(&first,FixedMetrics::default().0,&mut svg);let svg=svg.finish(first.bounds);
            assert!(svg.contains("height=\"1\" fill=\"#0000ff\""));
            sender.try_send(OwnerEvent::Evaluate{request_id:981,source:"(internal-set-lisp-face-attribute 'rune-org-link :underline nil)".into()}).unwrap();
            let step=worker.try_step().unwrap();assert_eq!(step.edit.unwrap(),true);assert!(step.render_error.is_none());
            let disabled=frame(&mut updates);assert_eq!(disabled.windows[0].rows[0].clusters[0].face.underline,None);
            for(a,b)in first.windows[0].rows[0].clusters.iter().zip(&disabled.windows[0].rows[0].clusters){assert_eq!(a.bounds,b.bounds);assert_eq!(a.source,b.source);}
            let before=worker.bridge().publication_revision().unwrap();
            sender.try_send(OwnerEvent::Evaluate{request_id:982,source:"(aset (internal-lisp-face-p 'rune-org-link (selected-frame)) 7 t)".into()}).unwrap();
            let step=worker.try_step().unwrap();assert_eq!(step.edit.unwrap(),true);assert!(step.render_error.is_none());
            let after=worker.bridge().publication_revision().unwrap();assert_eq!(before.0,after.0);assert_eq!(before.1,after.1);assert_ne!(before.2,after.2);
            assert_eq!(frame(&mut updates).windows[0].rows[0].clusters[0].face.underline,Some(Color(0,0,255,255)));
            assert_eq!(worker.bridge().snapshot().unwrap().sequence,sequence);assert_eq!(reader.latest().unwrap().input.as_ref().unwrap().identity(),identity);
            Ok(())
        }).unwrap();
    }
}
