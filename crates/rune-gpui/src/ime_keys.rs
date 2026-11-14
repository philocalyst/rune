//! Synchronous native IME key policy. Printable keys reach the platform only;
//! native fixed commands are consumed before platform text fallback can insert.
//! This mode deliberately does not guess at asynchronous typed keymap prefixes.
use rune_editor::Command;
#[derive(Debug)]
pub(crate) enum ImeKey {
    Platform,
    Paste,
    Command(Command),
    Cancel,
    Unsupported,
}
pub(crate) fn route(
    key: &str,
    control: bool,
    platform: bool,
    meta: bool,
    shift: bool,
    composing: bool,
) -> ImeKey {
    if (control && !meta && key == "g") || (!control && !meta && key == "escape") {
        return ImeKey::Cancel;
    }
    if composing {
        return ImeKey::Platform;
    }
    if meta {
        return ImeKey::Platform;
    }
    if control || platform {
        return match (key, shift) {
            ("v", _) => ImeKey::Paste,
            ("z", false) => ImeKey::Command(Command::Undo),
            ("z", true) | ("r", false) => ImeKey::Command(Command::Redo),
            ("space", _) => ImeKey::Platform,
            _ => ImeKey::Unsupported,
        };
    }
    if shift && matches!(key, "left" | "right" | "up" | "down" | "home" | "end") {
        return ImeKey::Unsupported;
    }
    match key {
        "left" => ImeKey::Command(Command::Move(-1)),
        "right" => ImeKey::Command(Command::Move(1)),
        "up" => ImeKey::Command(Command::MoveLines(-1)),
        "down" => ImeKey::Command(Command::MoveLines(1)),
        "home" => ImeKey::Command(Command::BeginningOfLine),
        "end" => ImeKey::Command(Command::EndOfLine),
        "backspace" => ImeKey::Command(Command::DeleteBackward(1)),
        "delete" => ImeKey::Command(Command::DeleteForward(1)),
        "enter" => ImeKey::Command(Command::Insert("\n".into())),
        "tab" => ImeKey::Command(Command::Insert("\t".into())),
        _ => ImeKey::Platform,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn printable_and_composing_keys_have_exactly_one_platform_source() {
        assert!(matches!(route("a", false, false, false, false, false), ImeKey::Platform));
        assert!(matches!(route("enter", false, false, false, false, true), ImeKey::Platform));
        assert!(matches!(route("backspace", false, false, false, false, true), ImeKey::Platform));
        assert!(
            matches!(route("enter",false,false,false,false,false),ImeKey::Command(Command::Insert(s)) if s=="\n")
        );
    }
    #[test]
    fn prefixes_are_not_half_armed_and_cancel_is_explicit() {
        assert!(matches!(route("x", true, false, false, false, false), ImeKey::Unsupported));
        assert!(matches!(
            route("z", false, true, false, false, false),
            ImeKey::Command(Command::Undo)
        ));
        assert!(matches!(route("2", false, false, false, false, false), ImeKey::Platform));
        assert!(matches!(route("g", true, false, false, false, true), ImeKey::Cancel));
        assert!(matches!(route("left", false, false, false, true, false), ImeKey::Unsupported));
    }
    #[test]
    fn clipboard_shortcut_is_explicit_only_outside_composition() {
        assert!(matches!(route("v", true, false, false, false, false), ImeKey::Paste));
        assert!(matches!(route("v", false, true, false, true, false), ImeKey::Paste));
        assert!(matches!(route("v", true, false, false, false, true), ImeKey::Platform));
        assert!(matches!(route("v", true, false, true, false, false), ImeKey::Platform));
    }
}
