//! Initial, callback-free selected-frame DEFAULT face color publication.
//! No text-property, overlay, inheritance, font, or decoration support is implied.
use super::*;
use rune_editor::display::{Color, Face};

fn color(value: Object, fallback: Color) -> std::result::Result<Color, &'static str> {
    if value.is_nil() || value == sym::UNSPECIFIED { return Ok(fallback); }
    let ObjectType::String(value) = value.untag() else { return Err("invalid default face color type"); };
    let value: &str = value;
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
/// Read only rooted tables; no lazy table creation, Lisp evaluation, or GC.
/// Recompute every owner turn so mutable vectors/strings cannot evade revision.
pub(crate) fn default_face(env: &Rt<Env>, cx: &Context) -> std::result::Result<Face, &'static str> {
    let frame = cx.selected_frame().map_err(|_|"no live owner frame for face publication")?;
    frame.validate(cx).map_err(|_|"invalid owner frame for face publication")?;
    let fallback = Face::default();
    let Some(table) = env.frame_face_tables.get(Object::from(frame)) else { return Ok(fallback); };
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
    let Some(value) = table.get(name.into()) else { return Ok(fallback); };
    let vector = checked_vector(value).map_err(|_|"invalid default face vector")?;
    Ok(Face { id: 0, foreground: color(vector[9].get(), fallback.foreground)?, background: color(vector[10].get(), fallback.background)? })
}
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
