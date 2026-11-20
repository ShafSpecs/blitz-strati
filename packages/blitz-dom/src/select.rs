//! `<select>` as a drop-down list (no `multiple`, no `size` above 1): which
//! option is selected, the closed control's label, and the picker an open
//! select draws over the page.
//!
//! Selectedness lives in the options' `selected` attributes: the last option
//! with one is selected, else the first enabled option. Choosing an option
//! moves the attribute and fires `input` then `change`, as browsers do.

use crate::node::TextBrush;
use crate::{BaseDocument, ElementData, local_name, qual_name};
use blitz_traits::events::{BlitzInputEvent, DomEvent, DomEventData};
use blitz_traits::node_id::NodeId;
use keyboard_types::{Key, NamedKey};
use kurbo::Rect;
use parley::{Layout, StyleProperty};

/// Horizontal padding (CSS px) of an option row in the picker.
pub const PICKER_PADDING: f64 = 8.0;
/// The extra indent (CSS px) of options inside an `<optgroup>`.
pub const PICKER_INDENT: f64 = 12.0;

/// The room (CSS px) the drop-down arrow takes at the end of the content box.
pub fn arrow_width(font_size: f32) -> f32 {
    (font_size * 1.25).round()
}

/// A drop-down select's state.
#[derive(Default)]
pub struct SelectData {
    /// The selected option's label in the select's font, at device scale.
    pub label: Option<Layout<TextBrush>>,
    /// The widest option label (CSS px): the select is sized to it.
    pub label_width: f32,
    /// The label's line height (CSS px).
    pub line_height: f32,
    /// The picker, while the select is open.
    pub picker: Option<Picker>,
}

// FIXME: parley layouts aren't Clone-able cheaply; a cloned select rebuilds.
impl Clone for SelectData {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// The list an open select shows.
pub struct Picker {
    /// Where it's drawn: CSS px, viewport (client) coordinates.
    pub rect: Rect,
    pub rows: Vec<PickerRow>,
    /// Row height (CSS px).
    pub row_height: f64,
    /// The highlighted row (an option, never a group label).
    pub highlighted: Option<usize>,
    /// How far the rows are scrolled (CSS px), when they don't all fit.
    pub scroll: f64,
}

pub struct PickerRow {
    /// The `<option>`, or the `<optgroup>` whose label this row shows.
    pub node: NodeId,
    pub label: Layout<TextBrush>,
    pub group_label: bool,
    pub disabled: bool,
    /// Inside an `<optgroup>`.
    pub indented: bool,
}

impl Picker {
    /// The row at a point (CSS px, viewport coordinates), if it's an option.
    pub fn option_at(&self, x: f64, y: f64) -> Option<usize> {
        if !self.rect.contains(kurbo::Point::new(x, y)) {
            return None;
        }
        let i = ((y - self.rect.y0 - 1.0 + self.scroll) / self.row_height).floor();
        let i = (i >= 0.0).then_some(i as usize)?;
        self.rows.get(i).filter(|r| !r.group_label).map(|_| i)
    }

    /// A row's rectangle (CSS px, viewport coordinates), scrolled.
    pub fn row_rect(&self, i: usize) -> Rect {
        let y0 = self.rect.y0 + 1.0 + i as f64 * self.row_height - self.scroll;
        Rect::new(
            self.rect.x0 + 1.0,
            y0,
            self.rect.x1 - 1.0,
            y0 + self.row_height,
        )
    }

    fn scroll_into_view(&mut self, i: usize) {
        let visible = self.rect.height() - 2.0;
        let top = i as f64 * self.row_height;
        if top < self.scroll {
            self.scroll = top;
        } else if top + self.row_height > self.scroll + visible {
            self.scroll = top + self.row_height - visible;
        }
    }

    fn max_scroll(&self) -> f64 {
        (self.rows.len() as f64 * self.row_height - (self.rect.height() - 2.0)).max(0.0)
    }

    /// The next enabled option row from `from`, going `step` (±1) rows at a time.
    fn next_option(&self, from: Option<usize>, step: isize) -> Option<usize> {
        let n = self.rows.len() as isize;
        let mut i = match from {
            Some(i) => i as isize + step,
            None if step > 0 => 0,
            None => n - 1,
        };
        while (0..n).contains(&i) {
            let row = &self.rows[i as usize];
            if !row.group_label && !row.disabled {
                return Some(i as usize);
            }
            i += step;
        }
        None
    }
}

/// Whether an element is a select shown as a drop-down list.
pub fn is_drop_down(el: &ElementData) -> bool {
    el.name.local == local_name!("select")
        && el.attr(local_name!("multiple")).is_none()
        && el
            .attr_parsed::<u32>(local_name!("size"))
            .is_none_or(|s| s <= 1)
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl BaseDocument {
    /// The drop-down select a node is (or is inside of).
    pub fn drop_down_select_of(&self, node: NodeId) -> Option<NodeId> {
        let mut id = Some(node);
        while let Some(n) = id {
            let node = self.get_node(n)?;
            if node.element_data().is_some_and(is_drop_down) {
                return Some(n);
            }
            id = node.parent;
        }
        None
    }

    /// The select's options and option groups, in tree order: (node, is a
    /// group label, is inside a group).
    fn select_entries(&self, select: NodeId) -> Vec<(NodeId, bool, bool)> {
        let mut out = Vec::new();
        for &child in &self.nodes[select].children {
            let Some(el) = self.nodes[child].element_data() else {
                continue;
            };
            if el.name.local == local_name!("option") {
                out.push((child, false, false));
            } else if el.name.local == local_name!("optgroup") {
                out.push((child, true, false));
                for &grandchild in &self.nodes[child].children {
                    if self.nodes[grandchild]
                        .element_data()
                        .is_some_and(|e| e.name.local == local_name!("option"))
                    {
                        out.push((grandchild, false, true));
                    }
                }
            }
        }
        out
    }

    /// The select's options, in tree order.
    pub fn select_options(&self, select: NodeId) -> Vec<NodeId> {
        self.select_entries(select)
            .into_iter()
            .filter(|e| !e.1)
            .map(|e| e.0)
            .collect()
    }

    /// An option's label: its `label` attribute, else its text (an optgroup:
    /// its `label`).
    pub fn option_label(&self, option: NodeId) -> String {
        let node = &self.nodes[option];
        let attr = node
            .element_data()
            .and_then(|el| el.attr(local_name!("label")))
            .filter(|l| !l.is_empty());
        match attr {
            Some(label) => collapse_whitespace(label),
            None if node
                .element_data()
                .is_some_and(|e| e.name.local == local_name!("optgroup")) =>
            {
                String::new()
            }
            None => collapse_whitespace(&node.text_content()),
        }
    }

    /// An option's value: its `value` attribute, else its text.
    pub fn option_value(&self, option: NodeId) -> String {
        match self.nodes[option]
            .element_data()
            .and_then(|el| el.attr(local_name!("value")))
        {
            Some(value) => value.to_owned(),
            None => collapse_whitespace(&self.nodes[option].text_content()),
        }
    }

    /// Disabled itself, or in a disabled `<optgroup>`.
    pub fn option_disabled(&self, option: NodeId) -> bool {
        let disabled = |id: NodeId| {
            self.nodes[id]
                .element_data()
                .is_some_and(|e| e.attr(local_name!("disabled")).is_some())
        };
        disabled(option)
            || self.nodes[option].parent.is_some_and(|p| {
                self.nodes[p]
                    .element_data()
                    .is_some_and(|e| e.name.local == local_name!("optgroup"))
                    && disabled(p)
            })
    }

    /// The selected option: the last with a `selected` attribute, else the
    /// first enabled one.
    pub fn selected_option(&self, select: NodeId) -> Option<NodeId> {
        let options = self.select_options(select);
        options
            .iter()
            .rev()
            .find(|&&o| {
                self.nodes[o]
                    .element_data()
                    .is_some_and(|e| e.attr(local_name!("selected")).is_some())
            })
            .or_else(|| options.iter().find(|&&o| !self.option_disabled(o)))
            .copied()
    }

    /// The select's value: the selected option's value (empty if none).
    pub fn select_value(&self, select: NodeId) -> String {
        self.selected_option(select)
            .map(|o| self.option_value(o))
            .unwrap_or_default()
    }

    /// The id of the open select, if one is open.
    pub fn open_select(&self) -> Option<NodeId> {
        self.open_select
    }

    fn select_label_layout(
        &mut self,
        select: NodeId,
        text: &str,
        bold: bool,
    ) -> Option<Layout<TextBrush>> {
        let style = self.nodes[select].primary_styles()?;
        let parley_style = crate::stylo_to_parley::style(select, &style);
        let scale = self.viewport.scale();
        let font_ctx = &mut self.font_ctx.lock().unwrap();
        // A non-breaking space keeps an empty label one line tall.
        let text = if text.is_empty() { "\u{a0}" } else { text };
        let mut builder = self.layout_ctx.ranged_builder(font_ctx, text, scale, true);
        builder.push_default(StyleProperty::FontFamily(parley_style.font_family));
        builder.push_default(StyleProperty::FontSize(parley_style.font_size));
        builder.push_default(StyleProperty::FontWidth(parley_style.font_width));
        builder.push_default(StyleProperty::FontStyle(parley_style.font_style));
        builder.push_default(StyleProperty::FontWeight(if bold {
            parley::FontWeight::BOLD
        } else {
            parley_style.font_weight
        }));
        builder.push_default(StyleProperty::LineHeight(parley_style.line_height));
        builder.push_default(StyleProperty::LetterSpacing(parley_style.letter_spacing));
        builder.push_default(StyleProperty::Brush(parley_style.brush));
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        Some(layout)
    }

    /// Rebuilds a select's label and measures its options (after it's built,
    /// or when its options or selection change). Marks it for layout if its
    /// width changed.
    pub(crate) fn refresh_select(&mut self, select: NodeId) {
        if !self.nodes.contains_key(select)
            || !self.nodes[select].element_data().is_some_and(is_drop_down)
        {
            return;
        }
        let scale = self.viewport.scale();
        let mut widest = 0.0_f32;
        for (entry, group, _) in self.select_entries(select) {
            let label = self.option_label(entry);
            if let Some(layout) = self.select_label_layout(select, &label, group) {
                widest = widest.max(layout.full_width() / scale);
            }
        }
        let selected = self
            .selected_option(select)
            .map(|o| self.option_label(o))
            .unwrap_or_default();
        let label = self.select_label_layout(select, &selected, false);
        let line_height = label.as_ref().map_or(16.0, |l| l.height() / scale);
        let Some(el) = self.nodes[select].element_data_mut() else {
            return;
        };
        let crate::node::SpecialElementData::Select(data) = &mut el.special_data else {
            el.special_data = crate::node::SpecialElementData::Select(Box::default());
            return self.refresh_select(select);
        };
        let resized = data.label_width != widest || data.line_height != line_height;
        data.label = label;
        data.label_width = widest;
        data.line_height = line_height;
        if resized {
            self.nodes[select].invalidate_layout_cache();
            self.nodes[select].insert_damage(style::selector_parser::RestyleDamage::RELAYOUT);
            self.nodes[select].mark_ancestors_dirty();
        }
        self.shell_provider.request_redraw();
    }

    fn select_data_mut(&mut self, select: NodeId) -> Option<&mut SelectData> {
        match &mut self.nodes[select].element_data_mut()?.special_data {
            crate::node::SpecialElementData::Select(data) => Some(data),
            _ => None,
        }
    }

    /// Selects an option (its `selected` attribute; the others lose theirs)
    /// and, if the selection changed, fires `input` then `change`.
    pub(crate) fn choose_option(
        &mut self,
        select: NodeId,
        option: NodeId,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        if self.option_disabled(option) || self.selected_option(select) == Some(option) {
            return false;
        }
        let options = self.select_options(select);
        {
            let mut mutator = self.mutate();
            for o in options {
                if o == option {
                    mutator.set_attribute(o, qual_name!("selected"), "");
                } else {
                    mutator.clear_attribute(o, qual_name!("selected"));
                }
            }
        }
        self.refresh_select(select);
        let value = self.select_value(select);
        dispatch_event(DomEvent::new(
            select,
            DomEventData::Input(BlitzInputEvent {
                value: value.clone(),
            }),
        ));
        dispatch_event(DomEvent::new(
            select,
            DomEventData::Change(BlitzInputEvent { value }),
        ));
        true
    }

    /// Opens a select's picker below it (above, if there's more room there).
    pub(crate) fn open_drop_down(&mut self, select: NodeId) {
        if self.open_select.is_some() {
            self.close_drop_down();
        }
        let Some(r) = self.get_client_bounding_rect(select) else {
            return;
        };
        let scale = self.viewport.scale();
        let (vw, vh) = (
            self.viewport.window_size.0 as f64 / scale as f64,
            self.viewport.window_size.1 as f64 / scale as f64,
        );
        let selected = self.selected_option(select);
        let mut rows = Vec::new();
        let mut widest = 0.0_f64;
        for (node, group_label, indented) in self.select_entries(select) {
            let label_text = self.option_label(node);
            let Some(label) = self.select_label_layout(select, &label_text, group_label) else {
                continue;
            };
            widest = widest.max(
                label.full_width() as f64 / scale as f64
                    + if indented { PICKER_INDENT } else { 0.0 },
            );
            let disabled = !group_label && self.option_disabled(node);
            rows.push(PickerRow {
                node,
                label,
                group_label,
                disabled,
                indented,
            });
        }
        if rows.is_empty() {
            return;
        }
        let line = rows[0].label.height() as f64 / scale as f64;
        let row_height = (line + 4.0).round();
        let width = (r.width).max(widest + 2.0 * PICKER_PADDING + 2.0);
        let full = rows.len() as f64 * row_height + 2.0;
        let below = vh - (r.y + r.height);
        let above = r.y;
        let (y, height) = if full <= below || below >= above {
            (r.y + r.height, full.min(below.max(row_height + 2.0)))
        } else {
            let h = full.min(above);
            (r.y - h, h)
        };
        let x = r.x.min(vw - width).max(0.0);
        let highlighted = rows
            .iter()
            .position(|row| Some(row.node) == selected && !row.group_label);
        let mut picker = Picker {
            rect: Rect::new(x, y, x + width, y + height),
            rows,
            row_height,
            highlighted,
            scroll: 0.0,
        };
        if let Some(i) = highlighted {
            picker.scroll_into_view(i);
        }
        if let Some(data) = self.select_data_mut(select) {
            data.picker = Some(picker);
            self.open_select = Some(select);
            self.shell_provider.request_redraw();
        }
    }

    /// Closes the open select's picker, if one is open.
    pub(crate) fn close_drop_down(&mut self) {
        if let Some(select) = self.open_select.take() {
            if self.nodes.contains_key(select) {
                if let Some(data) = self.select_data_mut(select) {
                    data.picker = None;
                }
            }
            self.shell_provider.request_redraw();
        }
    }

    /// The open picker, if any.
    pub fn open_picker(&self) -> Option<(NodeId, &Picker)> {
        let select = self.open_select?;
        match &self.nodes.get(select)?.element_data()?.special_data {
            crate::node::SpecialElementData::Select(data) => {
                data.picker.as_ref().map(|p| (select, p))
            }
            _ => None,
        }
    }

    fn open_picker_mut(&mut self) -> Option<&mut Picker> {
        let select = self.open_select?;
        self.select_data_mut(select)?.picker.as_mut()
    }

    /// The open picker highlights the option under the pointer (client CSS
    /// px). Returns whether the pointer is over the picker.
    pub(crate) fn picker_hover(&mut self, x: f64, y: f64) -> bool {
        let Some(picker) = self.open_picker_mut() else {
            return false;
        };
        if !picker.rect.contains(kurbo::Point::new(x, y)) {
            return false;
        }
        if let Some(i) = picker.option_at(x, y).filter(|&i| !picker.rows[i].disabled) {
            if picker.highlighted != Some(i) {
                picker.highlighted = Some(i);
                self.shell_provider.request_redraw();
            }
        }
        true
    }

    /// Scrolls the open picker (CSS px). Returns whether the point is over it.
    pub(crate) fn picker_scroll(&mut self, x: f64, y: f64, dy: f64) -> bool {
        let Some(picker) = self.open_picker_mut() else {
            return false;
        };
        if !picker.rect.contains(kurbo::Point::new(x, y)) {
            return false;
        }
        picker.scroll = (picker.scroll + dy).clamp(0.0, picker.max_scroll());
        self.shell_provider.request_redraw();
        true
    }

    /// A release over the open picker: an enabled option is chosen and the
    /// picker closes. Returns whether the point is over the picker.
    pub(crate) fn picker_release(
        &mut self,
        x: f64,
        y: f64,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        let Some((select, picker)) = self.open_picker() else {
            return false;
        };
        if !picker.rect.contains(kurbo::Point::new(x, y)) {
            return false;
        }
        let chosen = picker
            .option_at(x, y)
            .filter(|&i| !picker.rows[i].disabled)
            .map(|i| picker.rows[i].node);
        if let Some(option) = chosen {
            self.close_drop_down();
            self.choose_option(select, option, dispatch_event);
        }
        true
    }

    /// Keys while a select's picker is open. Returns whether the key was used
    /// (the page doesn't see it).
    pub(crate) fn picker_key(
        &mut self,
        key: &Key,
        alt: bool,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        let Some((select, _)) = self.open_picker() else {
            return false;
        };
        let step = |doc: &mut Self, f: &dyn Fn(&Picker) -> Option<usize>| {
            if let Some(picker) = doc.open_picker_mut() {
                if let Some(i) = f(picker) {
                    picker.highlighted = Some(i);
                    picker.scroll_into_view(i);
                }
            }
            doc.shell_provider.request_redraw();
        };
        let page = |p: &Picker| ((p.rect.height() - 2.0) / p.row_height).floor().max(1.0) as isize;
        match key {
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown) if alt => {
                self.close_drop_down()
            }
            Key::Named(NamedKey::F4) | Key::Named(NamedKey::Escape) => self.close_drop_down(),
            Key::Named(NamedKey::ArrowDown) => step(self, &|p| p.next_option(p.highlighted, 1)),
            Key::Named(NamedKey::ArrowUp) => step(self, &|p| p.next_option(p.highlighted, -1)),
            Key::Named(NamedKey::Home) => step(self, &|p| p.next_option(None, 1)),
            Key::Named(NamedKey::End) => step(self, &|p| p.next_option(None, -1)),
            Key::Named(NamedKey::PageDown) => step(self, &|p| {
                let mut at = p.highlighted;
                for _ in 0..page(p) {
                    at = p.next_option(at, 1).or(at);
                }
                at
            }),
            Key::Named(NamedKey::PageUp) => step(self, &|p| {
                let mut at = p.highlighted;
                for _ in 0..page(p) {
                    at = p.next_option(at, -1).or(at);
                }
                at
            }),
            Key::Named(NamedKey::Enter) | Key::Character(_)
                if matches!(key, Key::Named(NamedKey::Enter))
                    || *key == Key::Character(" ".into()) =>
            {
                let chosen = self
                    .open_picker()
                    .and_then(|(_, p)| p.highlighted.map(|i| p.rows[i].node));
                self.close_drop_down();
                if let Some(option) = chosen {
                    self.choose_option(select, option, dispatch_event);
                }
            }
            Key::Named(NamedKey::Tab) => {
                // Closes, and the key goes on to move focus.
                self.close_drop_down();
                return false;
            }
            Key::Character(c) => {
                let c = c.to_lowercase();
                let labels: Vec<String> = self
                    .open_picker()
                    .map(|(_, p)| {
                        p.rows
                            .iter()
                            .map(|r| self.option_label(r.node).to_lowercase())
                            .collect()
                    })
                    .unwrap_or_default();
                step(self, &|p| {
                    let n = p.rows.len();
                    let start = p.highlighted.map_or(0, |i| i + 1);
                    (0..n).map(|k| (start + k) % n).find(|&i| {
                        !p.rows[i].group_label
                            && !p.rows[i].disabled
                            && labels[i].starts_with(c.as_str())
                    })
                });
            }
            _ => {}
        }
        true
    }

    /// Keys on a focused, closed drop-down select: Alt+Down, Alt+Up, F4 and
    /// Space open it; the arrows, Home and End change the selection, and a
    /// letter picks the next option it starts, as on Windows. Returns
    /// whether the key was used.
    pub(crate) fn select_key(
        &mut self,
        select: NodeId,
        key: &Key,
        alt: bool,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        let enabled: Vec<NodeId> = self
            .select_options(select)
            .into_iter()
            .filter(|&o| !self.option_disabled(o))
            .collect();
        let current = self
            .selected_option(select)
            .and_then(|s| enabled.iter().position(|&o| o == s));
        let target = match key {
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) if alt => {
                self.open_drop_down(select);
                return true;
            }
            Key::Named(NamedKey::F4) => {
                self.open_drop_down(select);
                return true;
            }
            Key::Character(c) if c == " " => {
                self.open_drop_down(select);
                return true;
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowRight) => current
                .map_or(Some(0), |i| {
                    Some((i + 1).min(enabled.len().saturating_sub(1)))
                }),
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowLeft) => {
                current.map_or(Some(0), |i| Some(i.saturating_sub(1)))
            }
            Key::Named(NamedKey::Home) => Some(0),
            Key::Named(NamedKey::End) => Some(enabled.len().saturating_sub(1)),
            Key::Character(c) => {
                let c = c.to_lowercase();
                let n = enabled.len();
                let start = current.map_or(0, |i| i + 1);
                (0..n).map(|k| (start + k) % n).find(|&i| {
                    self.option_label(enabled[i])
                        .to_lowercase()
                        .starts_with(c.as_str())
                })
            }
            _ => return false,
        };
        if let Some(option) = target.and_then(|i| enabled.get(i).copied()) {
            self.choose_option(select, option, dispatch_event);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{Attribute, BaseDocument, DocumentConfig, QualName, qual_name};
    use blitz_traits::events::{DomEvent, DomEventData};
    use blitz_traits::node_id::NodeId;
    use blitz_traits::shell::{ColorScheme, Viewport};
    use keyboard_types::{Key, NamedKey};

    /// A document with one select; returns the select and its option ids.
    fn select_doc(options: &[(&str, &[(&str, &str)])]) -> (BaseDocument, NodeId, Vec<NodeId>) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let attr = |k: &str, v: &str| Attribute {
            name: QualName::new(None, markup5ever::ns!(), markup5ever::LocalName::from(k)),
            value: v.to_owned(),
        };
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html", html), vec![]);
        let body = m.create_element(qual_name!("body", html), vec![]);
        // A flex row: geometry without inline (font) layout.
        let row = m.create_element(qual_name!("div", html), vec![attr("style", "display:flex")]);
        let select = m.create_element(qual_name!("select", html), vec![]);
        let mut ids = Vec::new();
        for (text, attrs) in options {
            let o = m.create_element(
                qual_name!("option", html),
                attrs.iter().map(|(k, v)| attr(k, v)).collect(),
            );
            let t = m.create_text_node(text);
            m.append_children(o, &[t]);
            ids.push(o);
        }
        m.append_children(select, &ids);
        m.append_children(row, &[select]);
        m.append_children(body, &[row]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);
        (doc, select, ids)
    }

    fn events(f: impl FnOnce(&mut dyn FnMut(DomEvent))) -> Vec<(String, String)> {
        let mut out = Vec::new();
        f(&mut |e: DomEvent| {
            let v = match &e.data {
                DomEventData::Input(i) | DomEventData::Change(i) => i.value.clone(),
                _ => String::new(),
            };
            out.push((e.data.name().to_owned(), v));
        });
        out
    }

    #[test]
    fn selectedness_and_values() {
        let (doc, select, o) = select_doc(&[
            ("One", &[("disabled", "")]),
            ("Two", &[("value", "2")]),
            ("  Three  ", &[]),
        ]);
        // No `selected`: the first enabled option.
        assert_eq!(doc.selected_option(select), Some(o[1]));
        assert_eq!(doc.select_value(select), "2");
        assert_eq!(doc.option_value(o[2]), "Three");
        let (doc, select, o) = select_doc(&[
            ("A", &[("selected", "")]),
            ("B", &[("selected", "")]),
            ("C", &[]),
        ]);
        // The last `selected` wins.
        assert_eq!(doc.selected_option(select), Some(o[1]));
    }

    #[test]
    fn keys_on_a_closed_select_change_it_and_fire_input_then_change() {
        let (mut doc, select, o) = select_doc(&[
            ("Apple", &[]),
            ("Banana", &[("disabled", "")]),
            ("Cherry", &[]),
        ]);
        let fired = events(|d| {
            doc.select_key(select, &Key::Named(NamedKey::ArrowDown), false, d);
        });
        assert_eq!(
            doc.selected_option(select),
            Some(o[2]),
            "skips the disabled option"
        );
        assert_eq!(
            fired,
            [
                ("input".to_owned(), "Cherry".to_owned()),
                ("change".to_owned(), "Cherry".to_owned())
            ]
        );
        let fired = events(|d| {
            doc.select_key(select, &Key::Named(NamedKey::ArrowDown), false, d);
        });
        assert!(
            fired.is_empty(),
            "already at the last option: nothing fires"
        );
        events(|d| {
            doc.select_key(select, &Key::Character("a".into()), false, d);
        });
        assert_eq!(doc.selected_option(select), Some(o[0]), "typeahead");
    }

    #[test]
    fn the_picker_opens_highlights_and_chooses() {
        let (mut doc, select, o) = select_doc(&[("Apple", &[]), ("Banana", &[]), ("Cherry", &[])]);
        events(|d| {
            assert!(doc.select_key(select, &Key::Named(NamedKey::F4), false, d));
        });
        let (_, picker) = doc.open_picker().expect("open");
        assert_eq!(
            picker.highlighted,
            Some(0),
            "the selected option is highlighted"
        );
        let r = picker.row_rect(2);
        let fired = events(|d| {
            doc.picker_key(&Key::Named(NamedKey::ArrowDown), false, d);
        });
        assert!(fired.is_empty(), "moving the highlight changes nothing yet");
        assert_eq!(doc.open_picker().unwrap().1.highlighted, Some(1));
        // A release over Cherry chooses it and closes the picker.
        let fired = events(|d| {
            assert!(doc.picker_release(r.center().x, r.center().y, d));
        });
        assert!(doc.open_picker().is_none());
        assert_eq!(doc.selected_option(select), Some(o[2]));
        assert_eq!(fired.len(), 2);
        // Escape closes without changing anything.
        events(|d| {
            doc.select_key(select, &Key::Named(NamedKey::F4), false, d);
            doc.picker_key(&Key::Named(NamedKey::ArrowUp), false, d);
            doc.picker_key(&Key::Named(NamedKey::Escape), false, d);
        });
        assert!(doc.open_picker().is_none());
        assert_eq!(doc.selected_option(select), Some(o[2]));
    }
}
