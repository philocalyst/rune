//! Native evaluator overlay; API checks are not OS/window verification.
use crate::native_command_model::{CommandDraft, SaveShortcut, save_shortcut};
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, Pixels, Point, Render, ShapedLine, SharedString, TextRun, UTF16Selection,
    UnderlineStyle, Window, canvas, div, fill, point, prelude::*, px, rgb, rgba, size,
};
use rune_gpui::{
    FrameView,
    editor_transport::{EvaluationOutcome, SaveReply},
    evaluation_quit::EvaluationQuit,
};
use std::{
    collections::VecDeque,
    ops::Range,
    sync::{Arc, Mutex},
};

/// Private producer bound: at most64 startup sources plus one manual evaluation
/// and one save pending. Notices coalesce separately and cannot erase an explicit outcome.
#[derive(Clone)]
pub(crate) struct Reports(Arc<Mutex<ReportState>>, rune_display::HostPublisher);
#[derive(Default)]
struct ReportState {
    outcomes: VecDeque<TerminalOutcome>,
    notice: Option<String>,
}
enum TerminalOutcome {
    Evaluation(EvaluationOutcome),
    Save(SaveReply),
}
impl Reports {
    pub(crate) fn channel() -> (Self, rune_display::HostUpdates) {
        let (wake, updates) = rune_display::host_updates();
        (Self(Arc::new(Mutex::new(ReportState::default())), wake), updates)
    }
    pub(crate) fn evaluation(&self, mut outcome: EvaluationOutcome) {
        if let Err(error) = &mut outcome.outcome {
            *error = short(error);
        }
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .outcomes
            .push_back(TerminalOutcome::Evaluation(outcome));
        let _ = self.1.input_capacity_available(); // Never wake under report lock.
    }
    pub(crate) fn save(&self, mut reply: SaveReply) {
        if let Err(error) = &mut reply.outcome {
            *error = short(error);
        }
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .outcomes
            .push_back(TerminalOutcome::Save(reply));
        let _ = self.1.input_capacity_available();
    }
    pub(crate) fn notice(&self, text: String) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).notice = Some(short(&text));
        let _ = self.1.input_capacity_available();
    }
}
fn short(text: &str) -> String {
    let mut result = String::new();
    let mut count = 0;
    let mut space = false;
    for c in text.chars().take(1024) {
        if count == 240 {
            break;
        }
        if c.is_whitespace() || c.is_control() {
            if !result.is_empty() {
                space = true;
            }
            continue;
        }
        if space {
            result.push(' ');
            count += 1;
            space = false;
            if count == 240 {
                break;
            }
        }
        result.push(c);
        count += 1;
    }
    result
}

fn save_status(id: u64, saved: &rune_gpui::editor_transport::VisitedSave) -> String {
    match saved.kind {
        rune_gpui::editor_transport::VisitedSaveKind::Saved => {
            format!("Saved #{id}: {} ({} bytes written)", saved.path.display(), saved.bytes)
        }
        rune_gpui::editor_transport::VisitedSaveKind::Unchanged => {
            format!("Save #{id} unchanged: {} (no write)", saved.path.display())
        }
    }
}

pub(crate) struct CommandInput {
    draft: CommandDraft,
    focus: FocusHandle,
    editor: Entity<FrameView>,
    reports: Reports,
    quit: EvaluationQuit,
    status: String,
    notice: Option<String>,
    history: VecDeque<String>,
    errors: u64,
    next_save: u64,
    saving: Option<u64>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_text: String,
    scroll: Pixels,
}
impl CommandInput {
    fn new(
        editor: Entity<FrameView>,
        reports: Reports,
        mut updates: rune_display::HostUpdates,
        quit: EvaluationQuit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |view, cx| {
            while updates.next().await.is_some() {
                if view
                    .update(cx, |view, cx| {
                        view.drain();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.observe(&editor, |this, editor, cx| {
            this.drain();
            if editor.read(cx).input_readiness() == rune_gpui::InputReadiness::Disconnected {
                this.status =
                    "Owner disconnected; accepted outcome may be unknown; no automatic replay"
                        .into();
            }
            cx.notify();
        })
        .detach();
        let mut this = Self {
            draft: CommandDraft::new(),
            focus: cx.focus_handle(),
            editor,
            reports,
            quit,
            status: "Enter evaluates once; Escape closes; Alt+Up recalls last source".into(),
            notice: None,
            history: VecDeque::new(),
            errors: 0,
            next_save: 1,
            saving: None,
            last_layout: None,
            last_bounds: None,
            last_text: String::new(),
            scroll: px(0.),
        };
        cx.on_blur(&this.focus, window, |this, window, cx| {
            this.draft.cancel_preedit();
            this.last_bounds = None;
            window.invalidate_character_coordinates();
            cx.notify();
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.draft.cancel_preedit();
                this.last_bounds = None;
            }
            window.invalidate_character_coordinates();
            cx.notify();
        })
        .detach();
        cx.observe_window_bounds(window, |this, window, cx| {
            this.draft.cancel_preedit();
            this.last_bounds = None;
            window.invalidate_character_coordinates();
            cx.notify();
        })
        .detach();
        this.drain();
        this
    }
    fn enabled(&self, window: &Window) -> bool {
        self.focus.is_focused(window) && window.is_window_active()
    }
    fn drain(&mut self) {
        let (outcomes, notice) = {
            let mut reports = self.reports.0.lock().unwrap_or_else(|e| e.into_inner());
            (reports.outcomes.drain(..).collect::<Vec<_>>(), reports.notice.take())
        };
        for terminal in outcomes {
            self.status = match terminal {
                TerminalOutcome::Evaluation(outcome) => {
                    if outcome.outcome.is_err() {
                        self.errors = self.errors.saturating_add(1);
                    }
                    self.draft.complete(outcome.request_id);
                    match outcome.outcome {
                        Ok(()) => format!("#{} completed", outcome.request_id),
                        Err(error) => format!(
                            "#{}: {} (Alt+Up recalls source)",
                            outcome.request_id,
                            short(&error)
                        ),
                    }
                }
                TerminalOutcome::Save(reply) => {
                    if self.saving == Some(reply.request_id) {
                        self.saving = None;
                    }
                    match reply.outcome {
                        Ok(saved) => save_status(reply.request_id, &saved),
                        Err(error) => {
                            self.errors = self.errors.saturating_add(1);
                            format!("Save #{}: {} (not replayed)", reply.request_id, short(&error))
                        }
                    }
                }
            };
            if self.history.len() == 8 {
                self.history.pop_front();
            }
            self.history.push_back(short(&self.status));
        }
        if let Some(notice) = notice {
            self.notice = Some(notice);
        }
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            self.status = "Save outcome pending; not resubmitted".into();
            cx.notify();
            return;
        }
        let id = self.next_save;
        let Some(next) = id.checked_add(1) else {
            self.status = "Save request IDs exhausted".into();
            cx.notify();
            return;
        };
        self.next_save = next;
        // Same FIFO, selected visited file only; never a UI-chosen path.
        self.saving = Some(id);
        let accepted = self
            .editor
            .update(cx, |editor, _| editor.submit_visited_save(id, 16 * 1024 * 1024));
        self.status = if accepted {
            format!("Save #{id} pending")
        } else {
            format!("Save #{id} not admitted; awaiting rejection, no automatic retry")
        };
        cx.notify();
    }
    fn submit(&mut self, cx: &mut Context<Self>) {
        match self.draft.prepare() {
            Err(error) => self.status = error.into(),
            Ok(submission) => {
                let accepted = self.editor.update(cx, |editor, _| {
                    editor.submit_evaluation(submission.id, submission.source.clone())
                });
                if accepted {
                    self.notice = None;
                    self.status =
                        format!("#{} pending; C-g requests cooperative quit", submission.id);
                    self.draft.admitted(submission);
                } else {
                    self.draft.rejected_attempt(submission.id);
                    self.status =
                        "Owner did not accept source; draft retained, no automatic retry".into();
                }
            }
        }
        cx.notify();
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let command = (k.modifiers.control || k.modifiers.platform) && !k.modifiers.alt;
        if command && k.key == "g" {
            self.draft.cancel_preedit();
            self.status = if self.quit.request() {
                "Cooperative quit requested"
            } else {
                "No active evaluation to interrupt"
            }
            .into();
        } else if k.key == "escape" {
            self.draft.cancel_preedit();
        } else if self.draft.composing() {
            return;
        } else if k.modifiers.alt && k.key == "up" {
            if let Err(e) = self.draft.recall() {
                self.status = e.into();
            }
        } else if command {
            match k.key.as_str() {
                "a" => {
                    let _ = self.draft.select(0..self.draft.utf16_len());
                }
                "c" | "x" => {
                    let range = self.draft.selection();
                    if !range.is_empty() {
                        if let Some((_, text)) = self.draft.read(range) {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            if k.key == "x" {
                                let _ = self.draft.replace(None, "", None);
                            }
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        if let Err(e) = self.draft.replace(None, &text, None) {
                            self.status = e.into();
                        }
                    }
                }
                _ => self.status = "Unsupported evaluator-field shortcut".into(),
            }
        } else if k.modifiers.alt {
            return;
        } else {
            match k.key.as_str() {
                "enter" => {
                    if event.is_held {
                        window.prevent_default();
                        cx.stop_propagation();
                        return;
                    }
                    if k.modifiers.shift {
                        if let Err(e) = self.draft.replace(None, "\n", None) {
                            self.status = e.into();
                        }
                    } else {
                        self.submit(cx);
                    }
                }
                "left" | "right" => {
                    if k.modifiers.shift {
                        self.status =
                            "Shift-selection not yet supported; use Select All or pointer".into();
                    } else {
                        self.draft.move_cursor(if k.key == "left" { -1 } else { 1 });
                    }
                }
                "home" => {
                    let _ = self.draft.select(0..0);
                }
                "end" => {
                    let n = self.draft.utf16_len();
                    let _ = self.draft.select(n..n);
                }
                "backspace" | "delete" => {
                    if let Err(e) = self.draft.delete(k.key == "backspace") {
                        self.status = e.into();
                    }
                }
                _ => return, // Printable/dead keys belong to the platform text callback only.
            }
        }
        window.prevent_default();
        cx.stop_propagation();
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn key_up(&mut self, _: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.draft.release_confirmation();
        cx.notify();
    }
    fn geometry(&self) -> Option<(&ShapedLine, Bounds<Pixels>)> {
        if self.last_text != self.draft.text() {
            return None;
        }
        Some((self.last_layout.as_ref()?, self.last_bounds?))
    }
    fn click(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        if !self.draft.composing() {
            if let Some((line, bounds)) = self.geometry() {
                if bounds.contains(&event.position) {
                    let b =
                        line.closest_index_for_x(event.position.x - bounds.origin.x + self.scroll);
                    if let Some(p) = self.draft.utf16(b) {
                        let _ = self.draft.select(p..p);
                    }
                }
            }
        }
        window.invalidate_character_coordinates();
        cx.stop_propagation();
        cx.notify();
    }
}
impl EntityInputHandler for CommandInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if !self.enabled(window) {
            return None;
        }
        let (r, text) = self.draft.read(range)?;
        *adjusted = Some(r);
        Some(text)
    }
    fn selected_text_range(
        &mut self,
        ignore_disabled: bool,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !ignore_disabled && !self.enabled(window) {
            return None;
        }
        Some(UTF16Selection { range: self.draft.selection(), reversed: false })
    }
    fn marked_text_range(
        &self,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if !self.enabled(window) {
            return None;
        }
        self.draft.marked()
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled(window) {
            return;
        }
        self.draft.unmark();
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled(window) {
            self.status = "Late command text rejected after focus change; draft retained".into();
            cx.notify();
            return;
        }
        if let Err(e) = self.draft.replace(range, text, None) {
            self.status = e.into();
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled(window) {
            self.status = "Late preedit rejected after focus change; draft retained".into();
            cx.notify();
            return;
        }
        if let Err(e) = self.draft.replace(range, text, Some(selection)) {
            self.status = e.into();
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if !self.enabled(window) {
            return None;
        }
        let a = self.draft.byte(range.start).ok()?;
        let b = self.draft.byte(range.end).ok()?;
        if a > b {
            return None;
        }
        let (line, bounds) = self.geometry()?;
        let x = line.x_for_index(a) - self.scroll;
        let end = line.x_for_index(b) - self.scroll;
        if x < px(0.) || end > bounds.size.width {
            return None;
        }
        Some(Bounds::new(
            point(bounds.origin.x + x, bounds.origin.y),
            size((end - x).max(px(1.)), bounds.size.height),
        ))
    }
    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if !self.enabled(window) {
            return None;
        }
        let (line, bounds) = self.geometry()?;
        if !bounds.contains(&p) {
            return None;
        }
        self.draft.utf16(line.index_for_x(p.x - bounds.origin.x + self.scroll)?)
    }
}
struct Prepared {
    line: ShapedLine,
    scroll: Pixels,
    selection: Range<usize>,
    focus: FocusHandle,
}
impl Render for CommandInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prepaint = cx.entity();
        let paint = prepaint.clone();
        div()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(rgb(0x202830))
            .text_color(rgb(0xf1f3f5))
            .border_1()
            .border_color(rgb(0x78909c))
            .font_family("monospace")
            .text_size(px(14.))
            .occlude()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::click))
            .child("Evaluate Lisp • Enter once • Shift+Enter newline • Esc close • Alt+Up recall")
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let input = prepaint.read(cx);
                        let raw = input.draft.text().to_owned();
                        let display: SharedString = raw
                            .chars()
                            .map(|c| if c.is_ascii_control() { ' ' } else { c })
                            .collect::<String>()
                            .into();
                        let selection = input.draft.selection();
                        let cursor = input.draft.byte(selection.end).unwrap_or(0);
                        let style = window.text_style();
                        let run = TextRun {
                            len: display.len(),
                            font: style.font(),
                            color: rgb(0xf1f3f5).into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let runs = if let Some(marked) = input.draft.marked() {
                            let a = input.draft.byte(marked.start).unwrap();
                            let b = input.draft.byte(marked.end).unwrap();
                            vec![
                                TextRun { len: a, ..run.clone() },
                                TextRun {
                                    len: b - a,
                                    underline: Some(UnderlineStyle {
                                        color: None,
                                        thickness: px(1.),
                                        wavy: false,
                                    }),
                                    ..run.clone()
                                },
                                TextRun { len: display.len() - b, ..run },
                            ]
                            .into_iter()
                            .filter(|r| r.len > 0)
                            .collect::<Vec<_>>()
                        } else {
                            vec![run]
                        };
                        let line = window.text_system().shape_line(display, px(14.), &runs, None);
                        let x = line.x_for_index(cursor);
                        let mut scroll = input.scroll;
                        if x - scroll > bounds.size.width - px(8.) {
                            scroll = (x - bounds.size.width + px(8.)).max(px(0.));
                        }
                        if x < scroll {
                            scroll = x;
                        }
                        let focus = input.focus.clone();
                        let selection = input.draft.byte(selection.start).unwrap_or(0)..cursor;
                        prepaint.update(cx, |input, _| {
                            input.last_layout = Some(line.clone());
                            input.last_bounds = Some(bounds);
                            input.last_text = raw;
                            input.scroll = scroll;
                        });
                        Prepared { line, scroll, selection, focus }
                    },
                    move |bounds, prepared, window, cx| {
                        window.handle_input(
                            &prepared.focus,
                            ElementInputHandler::new(bounds, paint.clone()),
                            cx,
                        );
                        window.with_content_mask(Some(ContentMask { bounds }), |window| {
                            let x = prepared.line.x_for_index(prepared.selection.start)
                                - prepared.scroll;
                            let end =
                                prepared.line.x_for_index(prepared.selection.end) - prepared.scroll;
                            if !prepared.selection.is_empty() {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(bounds.origin.x + x, bounds.origin.y),
                                        size(end - x, bounds.size.height),
                                    ),
                                    rgba(0x4466aa88),
                                ));
                            }
                            let _ = prepared.line.paint(
                                point(bounds.origin.x - prepared.scroll, bounds.origin.y),
                                bounds.size.height,
                                window,
                                cx,
                            );
                            if prepared.selection.is_empty() && prepared.focus.is_focused(window) {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(bounds.origin.x + end, bounds.origin.y),
                                        size(px(1.), bounds.size.height),
                                    ),
                                    rgb(0xffffff),
                                ));
                            }
                        });
                    },
                )
                .w_full()
                .h(px(24.)),
            )
            .child(format!(
                "{}{}{} • {} request error(s)",
                short(&self.status),
                self.draft
                    .pending()
                    .map_or(String::new(), |id| format!(" • #{id} outcome pending")),
                self.saving.map_or(String::new(), |id| format!(" • save #{id} outcome pending")),
                self.errors
            ))
            .when_some(self.notice.clone(), |div, notice| {
                div.child(
                    gpui::div()
                        .text_color(rgb(0xffcc80))
                        .child(format!("Input/publication: {notice}")),
                )
            })
            .children(self.history.iter().rev().take(3).map(|outcome| {
                gpui::div().text_size(px(12.)).text_color(rgb(0xaabac6)).child(outcome.clone())
            }))
    }
}
pub(crate) struct NativeShell {
    editor: Entity<FrameView>,
    command: Entity<CommandInput>,
    open: bool,
}
impl NativeShell {
    pub(crate) fn new(
        editor: Entity<FrameView>,
        reports: Reports,
        updates: rune_display::HostUpdates,
        quit: EvaluationQuit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let command =
            cx.new(|cx| CommandInput::new(editor.clone(), reports, updates, quit, window, cx));
        cx.observe(&command, |_, _, cx| cx.notify()).detach();
        Self { editor, command, open: false }
    }
    fn capture(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if (k.modifiers.control || k.modifiers.platform) && !k.modifiers.alt && k.key == "enter" {
            self.open = true;
            window.focus(&self.command.read(cx).focus);
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match save_shortcut(
            &k.key,
            k.modifiers.control,
            k.modifiers.platform,
            k.modifiers.alt,
            k.modifiers.shift,
            event.is_held,
        ) {
            SaveShortcut::Unrelated => {}
            SaveShortcut::UnsupportedSaveAs => {
                self.command.update(cx, |command, cx| {
                    command.status =
                        "Save As is unsupported; this shortcut submitted no request and chose no path".into();
                    cx.notify();
                });
                window.prevent_default();
                cx.stop_propagation();
                return;
            }
            SaveShortcut::Held => {
                window.prevent_default();
                cx.stop_propagation();
                return;
            }
            SaveShortcut::Save => {
                if self.editor.read(cx).platform_text_unsettled() {
                    self.command.update(cx,|command,cx|{command.status="Confirm or resolve document composition before saving; no save submitted".into();cx.notify();});
                    if !self.editor.read(cx).has_platform_preedit() {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                    return;
                }
                self.command.update(cx, |command, cx| command.save(cx));
                window.prevent_default();
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        if self.open && k.key == "escape" && !self.command.read(cx).draft.composing() {
            self.open = false;
            window.focus(&self.editor.read(cx).focus_handle());
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
        }
    }
}
impl Render for NativeShell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let command = self.command.clone();
        let label = if self.open { "Close evaluator" } else { "Evaluate Lisp (Ctrl+Enter)" };
        div()
            .size_full()
            .relative()
            .capture_key_down(cx.listener(Self::capture))
            .child(self.editor.clone())
            .child(
                div()
                    .absolute()
                    .top(px(4.))
                    .right(px(4.))
                    .max_w(px(560.))
                    .max_h(px(28.))
                    .overflow_hidden()
                    .occlude()
                    .px_2()
                    .py_1()
                    .bg(rgb(0x263238))
                    .text_color(rgb(0xf1f3f5))
                    .text_size(px(12.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.open = !this.open;
                            if this.open {
                                window.focus(&this.command.read(cx).focus);
                            } else {
                                window.focus(&this.editor.read(cx).focus_handle());
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(label)
                    .child(format!(
                        " • {}",
                        self.command.read(cx).notice.as_ref().map_or_else(
                            || short(&self.command.read(cx).status),
                            |notice| format!("Notice: {}", short(notice))
                        )
                    )),
            )
            .when(self.open, |div| {
                div.child(
                    gpui::div().absolute().bottom(px(8.)).left(px(8.)).right(px(8.)).child(command),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        task::{Context as TaskContext, Poll, Waker},
    };
    #[test]
    fn reports_wake_without_document_frame_and_notices_do_not_erase_outcomes() {
        let (reports, mut updates) = Reports::channel();
        let mut next = std::pin::pin!(updates.next());
        let mut cx = TaskContext::from_waker(Waker::noop());
        assert!(matches!(next.as_mut().poll(&mut cx), Poll::Pending));
        reports.evaluation(EvaluationOutcome {
            request_id: 64,
            outcome: Err("evaluation error".into()),
        });
        reports.notice("later publication failure".into());
        assert!(
            matches!(next.as_mut().poll(&mut cx),Poll::Ready(Some(update)) if update.input_capacity && update.frame.is_none())
        );
        let state = reports.0.lock().unwrap();
        assert_eq!(state.outcomes.len(), 1);
        let TerminalOutcome::Evaluation(outcome) = &state.outcomes[0] else {
            panic!("wrong terminal kind")
        };
        assert_eq!(outcome.request_id, 64);
        assert_eq!(outcome.outcome.as_ref().unwrap_err(), "evaluation error");
        assert_eq!(state.notice.as_deref(), Some("later publication failure"));
    }
    #[test]
    fn report_payloads_are_bounded_and_keep_startup_plus_manual_outcomes() {
        let (reports, _) = Reports::channel();
        for id in 0..65 {
            reports
                .evaluation(EvaluationOutcome {
                    request_id: id, outcome: Err("λ".repeat(10_000))
                });
        }
        let state = reports.0.lock().unwrap();
        assert_eq!(state.outcomes.len(), 65);
        assert!(
            state
                .outcomes
                .iter()
                .all(|o| matches!(o,TerminalOutcome::Evaluation(outcome) if outcome.outcome.as_ref().unwrap_err().chars().count()<=240))
        );
        assert!(short(&" ".repeat(100_000)).is_empty());
    }
    #[test]
    fn manual_draft_runs_on_real_owner_fifo_and_returns_explicit_ui_outcome() {
        use rune_display::{FixedMetrics, Rect, host_updates};
        use rune_gpui::{
            editor_bridge::EditorBridge,
            editor_transport::{OwnerEvent, pending_editor_transport},
        };
        let (publisher, mut frames) = host_updates();
        let (sender, _replies, pending) = pending_editor_transport(4, publisher);
        let (reports, _report_updates) = Reports::channel();
        let owner_reports = reports.clone();
        let thread = std::thread::spawn(move || {
            crate::editor_document::with_lisp_runtime_owner(80, 24, |host| {
                let bridge = EditorBridge::new(
                    host,
                    FixedMetrics::default(),
                    Rect { x: 0., y: 0., width: 640., height: 480. },
                )
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                pending
                    .bind_initialized(bridge)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?
                    .with_evaluator(|host, source| host.evaluate_source(source))
                    .run_with_outcome(|step| {
                        if let Some(outcome) = step.evaluation {
                            owner_reports.evaluation(outcome);
                        }
                    })
                    .map_err(|e| anyhow::anyhow!("{e:?}"))
            })
            .unwrap();
        });
        let mut draft = CommandDraft::new();
        draft.replace(None, "(insert \"palette λ🙂\")", None).unwrap();
        let submission = draft.prepare().unwrap();
        sender
            .try_send(OwnerEvent::Evaluate {
                request_id: submission.id,
                source: submission.source.clone(),
            })
            .unwrap();
        draft.admitted(submission.clone());
        assert!(draft.prepare().is_err());
        drop(sender);
        thread.join().unwrap();
        let TerminalOutcome::Evaluation(result) =
            reports.0.lock().unwrap().outcomes.pop_front().unwrap()
        else {
            panic!("wrong terminal kind")
        };
        assert_eq!(result.request_id, submission.id);
        assert!(result.outcome.is_ok());
        assert!(draft.complete(result.request_id));
        assert!(draft.pending().is_none());
        let mut next = std::pin::pin!(frames.next());
        let mut cx = TaskContext::from_waker(Waker::noop());
        let Poll::Ready(Some(update)) = next.as_mut().poll(&mut cx) else {
            panic!("owner frame unavailable")
        };
        let frame = update.frame.unwrap();
        let rendered = frame
            .windows
            .iter()
            .flat_map(|w| w.rows.iter())
            .flat_map(|r| r.clusters.iter())
            .map(|c| c.text.as_ref())
            .collect::<String>();
        assert!(rendered.contains("palette λ🙂"));
    }
    #[test]
    fn evaluation_and_save_terminal_reports_preserve_arrival_order() {
        let (reports, _) = Reports::channel();
        reports.save(SaveReply { request_id: 1, outcome: Err("no visited file".into()) });
        reports.evaluation(EvaluationOutcome { request_id: 64, outcome: Ok(()) });
        let mut state = reports.0.lock().unwrap();
        assert!(matches!(
            state.outcomes.pop_front(),
            Some(TerminalOutcome::Save(SaveReply { request_id: 1, .. }))
        ));
        assert!(matches!(
            state.outcomes.pop_front(),
            Some(TerminalOutcome::Evaluation(EvaluationOutcome { request_id: 64, .. }))
        ));
    }
    #[test]
    fn saved_empty_and_unchanged_statuses_use_kind_not_byte_count() {
        use rune_gpui::editor_transport::{VisitedSave, VisitedSaveKind};
        let saved =
            VisitedSave { kind: VisitedSaveKind::Saved, path: "/owned/visited".into(), bytes: 0 };
        let unchanged = VisitedSave {
            kind: VisitedSaveKind::Unchanged,
            path: "/owned/visited".into(),
            bytes: 0,
        };
        let a = save_status(1, &saved);
        let b = save_status(2, &unchanged);
        assert!(a.starts_with("Saved #1"));
        assert!(b.contains("unchanged"));
        assert!(b.contains("no write"));
        assert_ne!(a, b);
    }
}
