//! A callback is a key only with a matching, unconsumed physical keydown.
//! Ambiguity fails closed to literal text. This is not durable OS event dedup.
#[derive(Default)]
pub(crate) struct PlainKeyProvenance { expected:Option<String> }
impl PlainKeyProvenance {
    pub(crate) fn keydown(&mut self,text:Option<&str>,allowed:bool) {
        self.expected=text.filter(|s|allowed&&s.chars().count()==1&&!s.chars().any(char::is_control)).map(str::to_owned);
    }
    pub(crate) fn take_callback(&mut self,unscoped:bool,text:&str)->bool {
        self.expected.take().is_some_and(|expected|unscoped&&expected==text)
    }
    pub(crate) fn invalidate(&mut self){self.expected=None;}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_one_matching_callback_per_physical_key_and_ambiguous_text_is_literal() {
        let mut p=PlainKeyProvenance::default();
        assert!(!p.take_callback(true,"x")); // menu paste/accessibility/IME without keydown
        p.keydown(Some("x"),true);assert!(p.take_callback(true,"x"));assert!(!p.take_callback(true,"x"));
        p.keydown(Some("x"),true);p.invalidate();assert!(!p.take_callback(true,"x")); // keyup/preedit/focus
        p.keydown(Some("x"),true);assert!(!p.take_callback(true,"y"));assert!(!p.take_callback(true,"x"));
        p.keydown(Some("x"),true);assert!(!p.take_callback(false,"x"));
        for _ in 0..3 {p.keydown(Some("x"),true);assert!(p.take_callback(true,"x"));} // repeated physical keydowns
        p.keydown(Some("x"),false);assert!(!p.take_callback(true,"x"));
        p.keydown(Some("xy"),true);assert!(!p.take_callback(true,"xy"));
    }
}
