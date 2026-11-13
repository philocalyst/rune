//! Deterministic headless painter. SVG text uses the same cluster positions as
//! the native adapter contract, not a separate HTML/text layout algorithm.
use crate::*;
use std::fmt::Write;
pub struct SvgTarget {
    body: String,
    clips: usize,
    next_clip: usize,
}
impl Default for SvgTarget {
    fn default() -> Self {
        Self { body: String::new(), clips: 0, next_clip: 0 }
    }
}
impl SvgTarget {
    pub fn finish(mut self, bounds: Rect) -> String {
        while self.clips > 0 {
            self.pop_clip();
        }
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"{} {} {} {}\">{}</svg>\n",
            bounds.width, bounds.height, bounds.x, bounds.y, bounds.width, bounds.height, self.body
        )
    }
}
fn color(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
impl PaintTarget for SvgTarget {
    fn push_clip(&mut self, r: Rect) {
        let id = self.next_clip;
        self.next_clip += 1;
        self.clips += 1;
        let _ = write!(
            self.body,
            "<defs><clipPath id=\"c{id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath></defs><g clip-path=\"url(#c{id})\">",
            r.x, r.y, r.width, r.height
        );
    }
    fn pop_clip(&mut self) {
        if self.clips > 0 {
            self.body.push_str("</g>");
            self.clips -= 1;
        }
    }
    fn fill(&mut self, r: Rect, c: Color) {
        let _ = write!(
            self.body,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\" fill-opacity=\"{}\"/>",
            r.x,
            r.y,
            r.width,
            r.height,
            color(c),
            f32::from(c.3) / 255.0
        );
    }
    fn text(&mut self, text: &str, p: Point, face: Face, m: FontMetrics) {
        let _ = write!(
            self.body,
            "<text x=\"{}\" y=\"{}\" fill=\"{}\" font-family=\"monospace\" font-size=\"{}\" xml:space=\"preserve\">{}</text>",
            p.x,
            p.y,
            color(face.foreground),
            m.ascent + m.descent,
            escape(text)
        );
    }
}
