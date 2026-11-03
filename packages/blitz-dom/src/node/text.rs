use blitz_traits::{
    events::{BlitzImeEvent, BlitzKeyEvent},
    node_id::NodeId,
    shell::ShellProvider,
};
use unicode_segmentation::UnicodeSegmentation;
use keyboard_types::{Key, Modifiers, NamedKey};
use parley::{ContentWidths, FontContext, LayoutContext};

use crate::util::ACTION_MOD;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
/// Parley Brush type for Blitz which contains the Blitz node id
pub struct TextBrush {
    /// The node id for the span
    pub id: NodeId,
}

impl TextBrush {
    pub(crate) fn from_id(id: NodeId) -> Self {
        Self { id }
    }
}

#[derive(Clone, Default)]
pub struct TextLayout {
    pub text: String,
    pub content_widths: Option<ContentWidths>,
    pub layout: parley::layout::Layout<TextBrush>,
    /// Block-axis offset (in CSS px) of the line boxes from the top of the container's content
    /// box, as applied by `align-content`.
    pub block_offset: f32,
    /// The out-of-flow positions for which some inline span with an inline box below it is a
    /// containing block (as `SPAN_*_CB` flags). The inline root claims those boxes in its place.
    /// Set by each `RunMode::PerformLayout` run of inline layout.
    pub span_cb_flags: stylo_taffy::StyleFlags,
}

impl TextLayout {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn content_widths(&mut self) -> ContentWidths {
        *self
            .content_widths
            .get_or_insert_with(|| self.layout.calculate_content_widths())
    }
}

impl std::fmt::Debug for TextLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextLayout")
    }
}

// TODO: support keypress events
pub enum GeneratedTextInputEvent {
    Input,
    Select,
    PreEditChange,
    Submit,
}

pub struct TextInputData {
    /// A parley TextEditor instance
    pub editor: Box<parley::PlainEditor<TextBrush>>,
    /// Whether the input is a singleline or multiline input
    pub is_multiline: bool,
    /// The scroll offset of the text content within the input, in CSS (unscaled) pixels.
    ///
    /// For single-line inputs this is a horizontal offset; for multi-line inputs it is a
    /// vertical offset. It is kept up to date so that the caret remains visible within the
    /// input's content box.
    pub scroll_offset: f32,
    /// The `placeholder` attribute's text, laid out with the `::placeholder`
    /// style. Painted while the input is empty.
    pub placeholder: Option<PlaceholderLayout>,
    /// Undo and redo (Ctrl+Z, Ctrl+Y / Ctrl+Shift+Z).
    pub history: EditHistory,
    /// An IME composition is in progress (between `compositionstart` and
    /// `compositionend`).
    pub composing: bool,
    /// A password field's value. The editor then holds a bullet per grapheme,
    /// so layout, caret, selection and hit testing never see the value.
    pub password: Option<String>,
}

const MASK: char = '•';

/// A bullet per grapheme of `text`.
pub(crate) fn masked(text: &str) -> String {
    MASK.to_string().repeat(text.graphemes(true).count())
}

/// A text input's undo history: the text and selection before each group of
/// edits. Consecutive edits of one kind (typing, or deleting) with no caret
/// move in between are one step, as in browsers; a paste, cut or line break
/// is a step of its own. Setting the value from script clears it.
#[derive(Default)]
pub struct EditHistory {
    undo: Vec<(String, parley::Selection)>,
    redo: Vec<(String, parley::Selection)>,
    group: Option<EditKind>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EditKind {
    Typing,
    Deleting,
    Other,
}

/// Undo steps kept per input.
const MAX_UNDO: usize = 100;

impl EditHistory {
    /// Call before an edit of `kind`.
    fn record(&mut self, kind: EditKind, editor: &parley::PlainEditor<TextBrush>) {
        if kind == EditKind::Other || self.group != Some(kind) {
            if self.undo.len() >= MAX_UNDO {
                self.undo.remove(0);
            }
            self.undo.push((editor.raw_text().to_owned(), editor.raw_selection().clone()));
        }
        self.redo.clear();
        self.group = (kind != EditKind::Other).then_some(kind);
    }

    /// A caret move or selection change ends the current group.
    pub fn break_group(&mut self) {
        self.group = None;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// The laid-out `placeholder` text of a text input.
#[derive(Clone)]
pub struct PlaceholderLayout {
    /// Laid out with the `::placeholder` font properties, at device scale.
    pub layout: parley::Layout<TextBrush>,
    /// The `::placeholder` `color`, with its `opacity` applied.
    pub color: crate::util::Color,
}

// FIXME: Implement Clone for PlainEditor
impl Clone for TextInputData {
    fn clone(&self) -> Self {
        TextInputData::new(self.is_multiline)
    }
}

impl TextInputData {
    pub fn new(is_multiline: bool) -> Self {
        let editor = Box::new(parley::PlainEditor::new(16.0));
        Self {
            editor,
            is_multiline,
            scroll_offset: 0.0,
            placeholder: None,
            history: EditHistory::default(),
            composing: false,
            password: None,
        }
    }

    /// The field's value (a password field's, not its bullets).
    pub fn value(&self) -> &str {
        self.password.as_deref().unwrap_or(self.editor.raw_text())
    }

    /// Masks the text (the field became a password field) or shows it,
    /// keeping the caret.
    pub fn set_masked(&mut self, font_ctx: &mut FontContext, layout_ctx: &mut LayoutContext<TextBrush>, masked: bool) {
        if masked == self.password.is_some() {
            return;
        }
        let caret = self.editor.raw_selection().focus().index();
        self.history.clear();
        if masked {
            let text = self.editor.raw_text().to_owned();
            let at = text[..caret.min(text.len())].graphemes(true).count();
            self.password = Some(text);
            self.remask(font_ctx, layout_ctx, at);
        } else {
            let text = self.password.take().unwrap_or_default();
            let at = text.grapheme_indices(true).nth(caret / MASK.len_utf8()).map_or(text.len(), |(i, _)| i);
            self.editor.set_text(&text);
            let mut driver = self.editor.driver(font_ctx, layout_ctx);
            driver.refresh_layout();
            driver.move_to_byte(at);
        }
    }

    /// Shows a bullet per grapheme of the password, the caret after `caret`
    /// graphemes.
    fn remask(&mut self, font_ctx: &mut FontContext, layout_ctx: &mut LayoutContext<TextBrush>, caret: usize) {
        let bullets = masked(self.password.as_deref().unwrap_or_default());
        let n = bullets.chars().count();
        self.editor.set_text(&bullets);
        let mut driver = self.editor.driver(font_ctx, layout_ctx);
        driver.refresh_layout();
        driver.move_to_byte(caret.min(n) * MASK.len_utf8());
    }

    /// Applies an edit of a password field's bullets to its value: `before`
    /// is the bullet count and the selection's start before it. An edit
    /// replaces bullets from where it starts and leaves the caret after what
    /// it inserted, so the removed graphemes and the inserted text follow.
    fn unmask_edit(&mut self, font_ctx: &mut FontContext, layout_ctx: &mut LayoutContext<TextBrush>, before: (usize, usize)) {
        let Some(value) = self.password.as_mut() else {
            return;
        };
        let (len, start) = before;
        let text = self.editor.raw_text();
        let focus = self.editor.raw_selection().focus().index().min(text.len());
        let chars: Vec<char> = text.chars().collect();
        let caret = text[..focus].chars().count();
        let at = start.min(caret);
        let inserted: String = chars[at..caret].iter().collect();
        let removed = (len + inserted.chars().count()).saturating_sub(chars.len());
        let graphemes: Vec<&str> = value.graphemes(true).collect();
        let head = graphemes[..at.min(graphemes.len())].concat() + &inserted;
        let tail = graphemes[(at + removed).min(graphemes.len())..].concat();
        let caret = head.graphemes(true).count();
        *value = head + &tail;
        self.remask(font_ctx, layout_ctx, caret);
    }

    pub fn set_text(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        text: &str,
    ) {
        if self.password.is_some() {
            if self.password.as_deref() != Some(text) {
                self.password = Some(text.to_owned());
                self.remask(font_ctx, layout_ctx, usize::MAX);
                self.history.clear();
            }
            return;
        }
        // While composing, the page has seen the text with the preedit in it
        // (input events carry it): setting that same text changes nothing,
        // and leaves the composition alone.
        let same = if self.editor.is_composing() {
            self.editor.raw_text() == text
        } else {
            self.editor.text() == text
        };
        if !same {
            self.editor.set_text(text);
            self.editor.driver(font_ctx, layout_ctx).refresh_layout();
            self.history.clear();
        }
    }

    /// Replaces the whole text, as an edit (an assistive technology setting
    /// the value): one undo step, the caret at the end. Returns whether the
    /// text changed.
    pub fn replace_all(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        text: &str,
    ) -> bool {
        if self.password.is_some() {
            if self.password.as_deref() == Some(text) {
                return false;
            }
            self.password = Some(text.to_owned());
            self.remask(font_ctx, layout_ctx, usize::MAX);
            return true;
        }
        if self.editor.raw_text() == text {
            return false;
        }
        self.history.record(EditKind::Other, &self.editor);
        let mut driver = self.editor.driver(font_ctx, layout_ctx);
        driver.select_all();
        driver.insert_or_replace_selection(text);
        true
    }

    /// Recompute [`Self::scroll_offset`] so that the caret stays visible within the input's
    /// content box.
    ///
    /// `content_box_width` and `content_box_height` are the dimensions of the input's content
    /// box in CSS (unscaled) pixels.
    pub fn clamp_scroll_offset(&mut self, content_box_width: f32, content_box_height: f32) {
        let Some(layout) = self.editor.try_layout() else {
            return;
        };
        // Parley lays out at the editor's scale, so its geometry is in scaled (device) pixels.
        // We convert into CSS (unscaled) pixels to match `scroll_offset` and the content box.
        let scale = layout.scale();

        // The caret geometry relative to the start of the text content.
        let Some(caret) = self.editor.cursor_geometry(1.5) else {
            return;
        };

        // Caret bounds and content/viewport extents along the scrolling axis (CSS pixels).
        let (caret_start, caret_end, content, viewport) = if self.is_multiline {
            (
                caret.y0 as f32 / scale,
                caret.y1 as f32 / scale,
                layout.height() / scale,
                content_box_height,
            )
        } else {
            (
                caret.x0 as f32 / scale,
                caret.x1 as f32 / scale,
                layout.full_width() / scale,
                content_box_width,
            )
        };

        let mut offset = self.scroll_offset;

        // Scroll so that both edges of the caret are within the visible region.
        if caret_end > offset + viewport {
            offset = caret_end - viewport;
        }
        if caret_start < offset {
            offset = caret_start;
        }

        // Never scroll past the content, and never scroll into negative space. The content
        // extent includes the caret so that a caret at the very end remains fully visible
        // (its rendered width extends slightly past the text).
        let max_offset = (content.max(caret_end) - viewport).max(0.0);
        self.scroll_offset = offset.clamp(0.0, max_offset);
    }

    /// The maximum valid value of [`Self::scroll_offset`] (in CSS pixels) given the input's
    /// content box, i.e. the extent by which the text content overflows the content box along
    /// the input's scroll axis.
    ///
    /// `content_box_width` and `content_box_height` are the dimensions of the input's content
    /// box in CSS (unscaled) pixels.
    pub fn max_scroll_offset(&self, content_box_width: f32, content_box_height: f32) -> f32 {
        let Some(layout) = self.editor.try_layout() else {
            return 0.0;
        };
        let scale = layout.scale();
        let (content, viewport) = if self.is_multiline {
            (layout.height() / scale, content_box_height)
        } else {
            (layout.full_width() / scale, content_box_width)
        };
        (content - viewport).max(0.0)
    }

    /// Scroll the input's text content by `delta` CSS pixels along its scroll axis (horizontal
    /// for single-line inputs, vertical for multi-line inputs), clamping to the scrollable
    /// range.
    ///
    /// Returns the portion of `delta` that could not be consumed (because the input was already
    /// scrolled to its limit), so the caller can bubble it up to an ancestor scroller.
    pub fn scroll_by(
        &mut self,
        delta: f32,
        content_box_width: f32,
        content_box_height: f32,
    ) -> f32 {
        let max_offset = self.max_scroll_offset(content_box_width, content_box_height);
        if max_offset <= 0.0 {
            return delta;
        }

        // Match the sign convention used for block scrolling: a positive delta decreases the
        // scroll offset.
        let new_offset = (self.scroll_offset - delta).clamp(0.0, max_offset);
        let consumed = self.scroll_offset - new_offset;
        self.scroll_offset = new_offset;
        delta - consumed
    }

    pub(crate) fn apply_keypress_event(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        event: BlitzKeyEvent,
    ) -> Option<GeneratedTextInputEvent> {
        // Do nothing if it is a keyup event
        if !event.state.is_pressed() {
            return None;
        }

        let mods = event.modifiers;
        let shift = mods.contains(Modifiers::SHIFT);
        let action_mod = mods.contains(ACTION_MOD);

        let is_multiline = self.is_multiline;

        if self.password.is_some() {
            // As in browsers, a password can't be copied, cut or undone.
            if let Key::Character(c) = &event.key
                && action_mod
                && matches!(c.to_lowercase().as_str(), "c" | "x" | "z" | "y")
            {
                return None;
            }
            let before = (
                self.editor.raw_text().chars().count(),
                self.editor.raw_selection().text_range().start / MASK.len_utf8(),
            );
            let generated = self.apply_editing_key(font_ctx, layout_ctx, shell_provider, event, is_multiline);
            if matches!(generated, Some(GeneratedTextInputEvent::Input)) {
                self.unmask_edit(font_ctx, layout_ctx, before);
            }
            return generated;
        }

        // Undo and redo
        if let Key::Character(c) = &event.key
            && action_mod
            && !mods.contains(Modifiers::ALT)
        {
            let c = c.to_lowercase();
            let (from, to) = match (c.as_str(), shift) {
                ("z", false) => (&mut self.history.undo, &mut self.history.redo),
                ("z", true) | ("y", false) => (&mut self.history.redo, &mut self.history.undo),
                _ => return self.apply_editing_key(font_ctx, layout_ctx, shell_provider, event, is_multiline),
            };
            let Some((text, selection)) = from.pop() else {
                return None;
            };
            to.push((self.editor.raw_text().to_owned(), self.editor.raw_selection().clone()));
            self.history.group = None;
            self.editor.set_text(&text);
            let mut driver = self.editor.driver(font_ctx, layout_ctx);
            driver.refresh_layout();
            driver.set_selection(selection);
            return Some(GeneratedTextInputEvent::Input);
        }
        self.apply_editing_key(font_ctx, layout_ctx, shell_provider, event, is_multiline)
    }

    fn apply_editing_key(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        event: BlitzKeyEvent,
        is_multiline: bool,
    ) -> Option<GeneratedTextInputEvent> {
        let mods = event.modifiers;
        let shift = mods.contains(Modifiers::SHIFT);
        let action_mod = mods.contains(ACTION_MOD);

        // Which kind of edit the key makes (for undo grouping), if any.
        let kind = match &event.key {
            Key::Character(c) if action_mod && matches!(c.to_lowercase().as_str(), "x" | "v") => Some(EditKind::Other),
            Key::Character(c) if c == "
" => is_multiline.then_some(EditKind::Other),
            Key::Named(NamedKey::Enter) => is_multiline.then_some(EditKind::Other),
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => Some(EditKind::Deleting),
            Key::Character(_) if !mods.contains(Modifiers::CONTROL) && !mods.contains(Modifiers::SUPER) => Some(EditKind::Typing),
            _ => None,
        };
        match kind {
            Some(kind) => self.history.record(kind, &self.editor),
            None => self.history.break_group(),
        }

        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);
        match event.key {
            Key::Character(c) if action_mod && matches!(c.as_str(), "c" | "x" | "v") => {
                match c.to_lowercase().as_str() {
                    "c" => {
                        if let Some(text) = driver.editor.selected_text() {
                            let _ = shell_provider.set_clipboard_text(text.to_owned());
                        }
                    }
                    "x" => {
                        if let Some(text) = driver.editor.selected_text() {
                            let _ = shell_provider.set_clipboard_text(text.to_owned());
                            driver.delete_selection()
                        }
                    }
                    "v" => {
                        let text = shell_provider.get_clipboard_text().unwrap_or_default();
                        driver.insert_or_replace_selection(&text)
                    }
                    _ => unreachable!(),
                }

                return Some(GeneratedTextInputEvent::Input);
            }
            Key::Character(c) if action_mod && matches!(c.to_lowercase().as_str(), "a") => {
                if shift {
                    driver.collapse_selection()
                } else {
                    driver.select_all()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if action_mod {
                    if shift {
                        driver.select_word_left()
                    } else {
                        driver.move_word_left()
                    }
                } else if shift {
                    driver.select_left()
                } else {
                    driver.move_left()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowRight) => {
                if action_mod {
                    if shift {
                        driver.select_word_right()
                    } else {
                        driver.move_word_right()
                    }
                } else if shift {
                    driver.select_right()
                } else {
                    driver.move_right()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowUp) => {
                if shift {
                    driver.select_up()
                } else {
                    driver.move_up()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowDown) => {
                if shift {
                    driver.select_down()
                } else {
                    driver.move_down()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::Home) => {
                if action_mod {
                    if shift {
                        driver.select_to_text_start()
                    } else {
                        driver.move_to_text_start()
                    }
                } else if shift {
                    driver.select_to_line_start()
                } else {
                    driver.move_to_line_start()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::End) => {
                if action_mod {
                    if shift {
                        driver.select_to_text_end()
                    } else {
                        driver.move_to_text_end()
                    }
                } else if shift {
                    driver.select_to_line_end()
                } else {
                    driver.move_to_line_end()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::Delete) => {
                if action_mod {
                    driver.delete_word()
                } else {
                    driver.delete()
                }
                return Some(GeneratedTextInputEvent::Input);
            }

            // On macOS this is handled by the apple standard keybindings
            #[cfg(not(target_os = "macos"))]
            Key::Named(NamedKey::Backspace) => {
                if action_mod {
                    driver.backdelete_word()
                } else {
                    driver.backdelete()
                }
                return Some(GeneratedTextInputEvent::Input);
            }

            Key::Character(c) if c == "\n" => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            Key::Named(NamedKey::Enter) => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            Key::Character(s)
                if !mods.contains(Modifiers::CONTROL) && !mods.contains(Modifiers::META) =>
            {
                driver.insert_or_replace_selection(&s);
                return Some(GeneratedTextInputEvent::Input);
            }
            _ => {}
        };

        None
    }

    pub(crate) fn apply_apple_standard_keybinding(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        command: &str,
    ) -> Option<GeneratedTextInputEvent> {
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);
        let is_multiline = self.is_multiline;

        match command {
            // Inserting Content

            // Inserts a backtab character.
            "insertBacktab:" => {}
            // Inserts a container break, such as a new page break.
            "insertContainerBreak:" => {}
            // Inserts a double quotation mark without substituting a curly quotation mark.
            "insertDoubleQuoteIgnoringSubstitution:" => {
                driver.insert_or_replace_selection("\"");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a line break character.
            "insertLineBreak:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a newline character.
            "insertNewline:" => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            // Inserts a newline character without invoking the field editor’s normal handling to end editing.
            "insertNewlineIgnoringFieldEditor:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a paragraph separator.
            "insertParagraphSeparator:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            "insertSingleQuoteIgnoringSubstitution:" => {
                driver.insert_or_replace_selection("'");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a tab character.
            "insertTab:" | "insertTabIgnoringFieldEditor:" => {
                // Ignore for now seeing as parley has poor support for laying out tabs
            }
            // Inserts the text you specify.
            "insertText:" => {}

            // Deleting Content

            // Deletes content moving backward from the current insertion point.
            // TODO: handle deleteBackwardByDecomposingPreviousCharacter separately
            "deleteBackward:" | "deleteBackwardByDecomposingPreviousCharacter:" => {
                driver.backdelete();
                return Some(GeneratedTextInputEvent::Input);
            }
            "deleteForward:" => {
                driver.delete();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the beginning of the current line.
            "deleteToBeginningOfLine:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_line_start();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the beginning of the current paragraph.
            "deleteToEndOfLine:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_line_end();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            "deleteToBeginningOfParagraph:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_hard_line_start();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }

            // Deletes content from the insertion point to the end of the current line.
            "deleteToEndOfParagraph:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_hard_line_end();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the end of the current paragraph.
            "deleteWordBackward:" => {
                driver.backdelete_word();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes the word preceding the current insertion point.
            "deleteWordForward:" => {
                driver.delete_word();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes the current selection, placing it in a temporary buffer, such as the Clipboard.
            "yank:" => {
                if let Some(text) = driver.editor.selected_text() {
                    let _ = shell_provider.set_clipboard_text(text.to_owned());
                    driver.delete_selection();
                    return Some(GeneratedTextInputEvent::Input);
                }
            }

            // Moving the Insertion Pointer

            // Moves the insertion pointer backward in the current content.
            "moveBackward:" => {
                driver.move_left(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moves the insertion pointer down in the current content.
            "moveDown:" => {
                driver.move_down();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer forward in the current content.
            "moveForward:" => {
                driver.move_right();
                return Some(GeneratedTextInputEvent::Select);
            } // TODO: Bidi-aware

            // Moves the insertion pointer left in the current content.
            "moveLeft:" => {
                driver.move_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer right in the current content.
            "moveRight:" => {
                driver.move_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer up in the current content.
            "moveUp:" => {
                driver.move_up();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Modifying the Selection

            // Extends the selection to include the content before the current selection.
            "moveBackwardAndModifySelection:" => {
                driver.select_left(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content below the current selection.
            "moveDownAndModifySelection:" => {
                driver.select_down();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content after the current selection.
            "moveForwardAndModifySelection:" => {
                driver.select_right(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content to the left of the current selection.
            "moveLeftAndModifySelection:" => {
                driver.select_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content to the right of the current selection.
            "moveRightAndModifySelection:" => {
                driver.select_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content above the current selection.
            "moveUpAndModifySelection:" => {
                driver.select_up();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Changing the Selection
            "selectAll:" => {
                driver.select_all();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectLine:" => {
                driver.move_to_line_start();
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectParagraph:" => {
                driver.move_to_hard_line_start();
                driver.select_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectWord:" => {
                // TODO
            }

            // Moving the Selection in Documents
            "moveToBeginningOfDocument:" => {
                driver.move_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfDocumentAndModifySelection:" => {
                driver.select_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfDocument:" => {
                driver.move_to_text_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfDocumentAndModifySelection:" => {
                driver.move_to_text_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection in Paragraphs
            "moveParagraphBackwardAndModifySelection:" => {}
            "moveParagraphForwardAndModifySelection:" => {}
            "moveToBeginningOfParagraph:" => {
                driver.move_to_hard_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfParagraphAndModifySelection:" => {
                driver.select_to_hard_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfParagraph:" => {
                driver.move_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfParagraphAndModifySelection:" => {
                driver.select_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection in Lines of Text
            "moveToBeginningOfLine:" => {
                driver.move_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfLineAndModifySelection:" => {
                driver.select_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfLine:" => {
                driver.move_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfLineAndModifySelection:" => {
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToLeftEndOfLine:" => {
                driver.move_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToLeftEndOfLineAndModifySelection:" => {
                driver.select_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToRightEndOfLine:" => {
                driver.move_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToRightEndOfLineAndModifySelection:" => {
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection by Word Boundaries
            "moveWordBackward:" => {
                driver.move_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordBackwardAndModifySelection:" => {
                driver.select_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordForward:" => {
                driver.move_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordForwardAndModifySelection:" => {
                driver.select_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordLeft:" => {
                driver.move_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordLeftAndModifySelection:" => {
                driver.select_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordRight:" => {
                driver.move_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordRightAndModifySelection:" => {
                driver.select_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Scrolling Content

            // Scrolls the content down by a page.
            "scrollPageDown:" => {}
            // Scrolls the content up by a page.
            "scrollPageUp:" => {}
            // Scrolls the content down by a line.
            "scrollLineDown:" => {}
            // Scrolls the content up by a line.
            "scrollLineUp:" => {}
            // Scrolls the content to the beginning of the document.
            "scrollToBeginningOfDocument:" => {}
            // Scrolls the content to the end of the document.
            "scrollToEndOfDocument:" => {}
            // Moves the visible content region down by a page.
            "pageDown:" => {}
            // Moves the visible content region up by a page.
            "pageUp:" => {}
            // Moves the visible content region down by a page, and extends the current selection.
            "pageDownAndModifySelection:" => {}
            // Moves the visible content region up by a page, and extends the current selection.
            "pageUpAndModifySelection:" => {}
            // Moves the visible content region so the current selection is visually centered.
            "centerSelectionInVisibleArea:" => {}

            // Transposing Elements

            // Transposes the content around the current selection.
            "transpose:" => {}
            // Transposes the words around the current selection.
            "transposeWords:" => {}

            // Indenting Content
            // Indents the content at the current selection.
            "indent:" => {}

            // Canceling Operations
            // Cancels the current operation.
            "cancelOperation:" => {}

            // Supporting QuickLook
            // Invokes QuickLook to preview the current selection.
            "quickLookPreviewItems:" => {}

            // Supporting Writing Directions
            "makeBaseWritingDirectionLeftToRight:" => {}
            "makeBaseWritingDirectionNatural:" => {}
            "makeBaseWritingDirectionRightToLeft:" => {}
            "makeTextWritingDirectionLeftToRight:" => {}
            "makeTextWritingDirectionNatural:" => {}
            "makeTextWritingDirectionRightToLeft:" => {}

            // Changing Capitalization
            "capitalizeWord:" => {}
            "changeCaseOfLetter:" => {}
            "lowercaseWord:" => {}
            "uppercaseWord:" => {}

            // Supporting Marked Selections
            "setMark:" => {}
            "selectToMark:" => {}
            "deleteToMark:" => {}
            "swapWithMark:" => {}

            // Supporting Autocomplete
            "complete:" => {}

            // Instance Methods
            "showContextMenuForSelection:" => {}

            // Unknown command
            _ => {}
        };

        None
    }

    pub(crate) fn apply_ime_event(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        event: BlitzImeEvent,
    ) -> Option<GeneratedTextInputEvent> {
        // A password field gets no IME (see `Node::wants_ime`).
        if self.password.is_some() {
            return None;
        }
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);

        match event {
            BlitzImeEvent::Enabled => {
                // Do nothing
                None
            }
            BlitzImeEvent::Disabled => {
                // A preedit left over (a cancelled composition) is removed,
                // and the page told the text changed.
                let had_preedit = driver.editor.is_composing();
                driver.clear_compose();
                Some(if had_preedit { GeneratedTextInputEvent::Input } else { GeneratedTextInputEvent::PreEditChange })
            }
            BlitzImeEvent::Commit(text) => {
                driver.insert_or_replace_selection(&text);
                Some(GeneratedTextInputEvent::Input)
            }
            // The text with the preedit in it is the input's value during a
            // composition (input events fire for each change, as in browsers),
            // so a page that controls the value keeps it.
            BlitzImeEvent::Preedit(text, cursor) => {
                if text.is_empty() {
                    if !driver.editor.is_composing() {
                        return Some(GeneratedTextInputEvent::PreEditChange);
                    }
                    driver.clear_compose();
                } else {
                    driver.set_compose(&text, cursor);
                }
                Some(GeneratedTextInputEvent::Input)
            }
            BlitzImeEvent::DeleteSurrounding {
                before_bytes,
                after_bytes,
            } => {
                let _ = before_bytes;
                let _ = after_bytes;
                // TODO
                None
            }
        }
    }
}

#[cfg(test)]
mod password_tests {
    use super::*;

    /// A password shows a bullet per grapheme; edits of the bullets edit the
    /// value; showing it again keeps the caret.
    #[test]
    fn a_password_is_masked_and_edited() {
        let (mut f, mut l) = (FontContext::new(), LayoutContext::new());
        let mut d = TextInputData::new(false);
        d.editor.set_scale(1.0);
        d.set_text(&mut f, &mut l, "ab");
        d.set_masked(&mut f, &mut l, true);
        assert_eq!((d.editor.raw_text(), d.value()), ("••", "ab"));
        d.set_text(&mut f, &mut l, "a👍🏽b");
        assert_eq!(d.editor.raw_text(), "•••");

        d.editor.driver(&mut f, &mut l).backdelete();
        d.unmask_edit(&mut f, &mut l, (3, 3));
        assert_eq!(d.value(), "a👍🏽");
        d.editor.driver(&mut f, &mut l).backdelete();
        d.unmask_edit(&mut f, &mut l, (2, 2));
        assert_eq!(d.value(), "a", "the whole emoji goes");

        let mut driver = d.editor.driver(&mut f, &mut l);
        driver.move_to_byte(0);
        driver.insert_or_replace_selection("xy");
        d.unmask_edit(&mut f, &mut l, (1, 0));
        assert_eq!((d.editor.raw_text(), d.value()), ("•••", "xya"));

        let mut driver = d.editor.driver(&mut f, &mut l);
        driver.select_byte_range(3, 9);
        driver.delete_selection();
        d.unmask_edit(&mut f, &mut l, (3, 1));
        assert_eq!(d.value(), "x");

        d.set_masked(&mut f, &mut l, false);
        assert_eq!((d.editor.raw_text(), d.password.as_deref()), ("x", None));
        assert_eq!(d.editor.raw_selection().focus().index(), 1);
    }

    /// A bullet typed or pasted over the bullets is part of the value, even
    /// when the bullets look unchanged; an edit that changes nothing keeps it.
    #[test]
    fn a_typed_bullet_is_part_of_the_password() {
        let (mut f, mut l) = (FontContext::new(), LayoutContext::new());
        let mut d = TextInputData::new(false);
        d.editor.set_scale(1.0);
        d.set_masked(&mut f, &mut l, true);
        d.set_text(&mut f, &mut l, "ab");

        let mut driver = d.editor.driver(&mut f, &mut l);
        driver.select_byte_range(0, 3);
        driver.insert_or_replace_selection("•");
        d.unmask_edit(&mut f, &mut l, (2, 0));
        assert_eq!((d.editor.raw_text(), d.value()), ("••", "•b"));

        let mut driver = d.editor.driver(&mut f, &mut l);
        driver.select_byte_range(0, 6);
        driver.insert_or_replace_selection("••");
        d.unmask_edit(&mut f, &mut l, (2, 0));
        assert_eq!((d.editor.raw_text(), d.value()), ("••", "••"));

        d.editor.driver(&mut f, &mut l).move_to_byte(0);
        d.editor.driver(&mut f, &mut l).backdelete();
        d.unmask_edit(&mut f, &mut l, (2, 0));
        assert_eq!(d.value(), "••");
    }
}
