use crate::{
    BaseDocument, ElementData, Node as BlitzDomNode, NodeData, NodeId as BlitzDomNodeId, local_name,
};
use accesskit::{
    Action, Affine, HasPopup, Node as AccessKitNode, NodeId, Rect, Role, Toggled, TreeId, TreeInfo,
    TreeUpdate,
};
use style::properties::longhands::visibility;
use unicode_segmentation::UnicodeSegmentation;

impl BaseDocument {
    pub fn build_accessibility_tree(&self) -> TreeUpdate {
        let mut nodes = std::collections::HashMap::new();
        let mut window = AccessKitNode::new(Role::Window);
        // Node bounds are in CSS pixels; the window maps them to device pixels.
        window.set_transform(Affine::scale(self.viewport.scale_f64()));
        let mut hidden_nodes = std::collections::HashSet::new();
        let mut labelled_by_nodes = Vec::new();
        let mut described_by_nodes = Vec::new();

        self.visit(|node_id, node| {
            if node.is_hidden_from_accessibility_tree()
                || node.parent.and_then(|p| nodes.get(&p)).is_some_and(|(_, parent): &(_, AccessKitNode)| {
                    is_text_input_role(parent.role()) && parent.value().is_some()
                })
                || node
                    .parent
                    .map(|p| hidden_nodes.contains(&p))
                    .unwrap_or(false)
            {
                hidden_nodes.insert(node_id);
                return;
            }
            let parent = node
                .parent
                .and_then(|parent_id| nodes.get_mut(&parent_id))
                .map(|(_, parent)| parent)
                .unwrap_or(&mut window);
            let (id, builder) = self.build_accessibility_node(node, parent);

            if node.attr(local_name!("aria-labelledby")).is_some() {
                labelled_by_nodes.push(node_id);
            }
            if node.attr(local_name!("aria-describedby")).is_some() {
                described_by_nodes.push(node_id);
            }
            nodes.insert(node_id, (id, builder));
        });

        let mut label = FlatText::default();
        for node_id in labelled_by_nodes {
            let Some(id_refs) = self.nodes[node_id].attr(local_name!("aria-labelledby")) else {
                continue;
            };
            let Some((id, mut builder)) = nodes.remove(&node_id) else {
                continue;
            };
            let mut has_target = false;
            label.clear();
            for target in self.referenced_dom_ids_to_node_ids(id_refs) {
                if !has_target {
                    builder.clear_labelled_by();
                    has_target = true;
                }
                if target == node_id || nodes.contains_key(&target) {
                    builder.push_labelled_by(NodeId(target.as_u64()));
                }
                self.nodes[target].write_text_alternative(&mut label);
                label.break_word();
            }
            if !label.text.is_empty() {
                builder.set_label(label.text.as_str());
            }
            nodes.insert(node_id, (id, builder));
        }

        for node_id in described_by_nodes {
            let Some(id_refs) = self.nodes[node_id].attr(local_name!("aria-describedby")) else {
                continue;
            };
            let Some((id, mut builder)) = nodes.remove(&node_id) else {
                continue;
            };
            for target in self.referenced_dom_ids_to_node_ids(id_refs) {
                if target == node_id || nodes.contains_key(&target) {
                    builder.push_described_by(NodeId(target.as_u64()));
                }
            }
            nodes.insert(node_id, (id, builder));
        }

        // An open select's picker: its options, where they're drawn.
        if let Some((select, picker)) = self.open_picker() {
            for (i, row) in picker.rows.iter().enumerate().filter(|(_, r)| !r.group_label) {
                let mut option = AccessKitNode::new(Role::ListBoxOption);
                option.set_label(self.option_label(row.node));
                let r = picker.row_rect(i);
                option.set_bounds(Rect { x0: r.x0, y0: r.y0, x1: r.x1, y1: r.y1 });
                option.set_selected(picker.highlighted == Some(i));
                if row.disabled {
                    option.set_disabled();
                } else {
                    option.add_action(Action::Click);
                }
                let id = NodeId(row.node.as_u64());
                if let Some((_, parent)) = nodes.get_mut(&select) {
                    parent.push_child(id);
                }
                nodes.insert(row.node, (id, option));
            }
        }

        let mut nodes: Vec<_> = nodes
            .into_iter()
            .map(|(_, (id, node))| (id, node))
            .collect();
        let runs = add_input_text_runs(&mut nodes);
        for (input_id, run_id) in runs {
            let Some(input) = self.get_node(BlitzDomNodeId::from_u64(input_id.0))
                .and_then(|node| node.element_data())
                .and_then(|element| element.text_input_data()) else { continue };
            let selection = input.editor.raw_selection();
            let value = input.editor.raw_text();
            let position = |byte_index| accesskit::TextPosition {
                node: run_id,
                character_index: text_character_index(value, byte_index),
            };
            if let Some((_, node)) = nodes.iter_mut().find(|(id, _)| *id == input_id) {
                node.set_text_selection(accesskit::TextSelection {
                    anchor: position(selection.anchor().index()),
                    focus: position(selection.focus().index()),
                });
            }
        }
        nodes.push((NodeId(u64::MAX), window));

        let tree = TreeInfo::new(NodeId(u64::MAX));
        TreeUpdate {
            tree_id: TreeId::ROOT,
            nodes,
            tree: Some(tree),
            focus: NodeId(self.focus_node_id.map(|id| id.as_u64()).unwrap_or(u64::MAX)),
        }
    }

    fn referenced_dom_ids_to_node_ids<'a>(
        &'a self,
        nodes_reference: &'a str,
    ) -> impl Iterator<Item = BlitzDomNodeId> + 'a {
        nodes_reference
            .split_ascii_whitespace()
            .filter_map(|dom_id| self.nodes_to_id.get(dom_id)?.first().copied())
    }

    fn build_accessibility_node(
        &self,
        node: &BlitzDomNode,
        parent: &mut AccessKitNode,
    ) -> (NodeId, AccessKitNode) {
        let id = NodeId(node.id.as_u64());

        let mut builder = AccessKitNode::default();
        if node.parent.is_none() {
            builder.set_role(Role::Window);
            let scale = self.viewport.scale_f64();
            builder.set_bounds(Rect::new(
                0.0,
                0.0,
                self.viewport.window_size.0 as f64 / scale,
                self.viewport.window_size.1 as f64 / scale,
            ));
        } else if let Some(element_data) = node.element_data() {
            let name = element_data.name.local.to_string();
            let role_attr = element_data.attr(local_name!("role"));

            // TODO: The roles of elements with strong native semantics cannot be overridden; see
            // https://www.w3.org/TR/wai-aria-1.2/#host_general_conflict.
            let role = role_attr
                .and_then(role_from_name)
                .or_else(|| role_from_element_data(element_data))
                .unwrap_or(Role::Unknown);

            builder.set_role(role);

            if let Some(label) = self.accessible_name(node, element_data, role) {
                builder.set_label(label);
            }
            if let Some(rect) = self.get_client_bounding_rect(node.id) {
                if rect.width > 0.0 || rect.height > 0.0 {
                    builder.set_bounds(Rect {
                        x0: rect.x,
                        y0: rect.y,
                        x1: rect.x + rect.width,
                        y1: rect.y + rect.height,
                    });
                }
            }

            // https://www.w3.org/TR/wai-aria-1.2/#tree_exclusion
            if element_data.attr(local_name!("aria-hidden")) == Some("true") {
                builder.set_hidden();
            }

            if let Some(aria_expanded) = element_data.attr(local_name!("aria-expanded"))
                && let Ok(aria_expanded) = aria_expanded.parse()
            {
                builder.set_expanded(aria_expanded);
            }

            if element_data.attr(local_name!("aria-disabled")) == Some("true") {
                builder.set_disabled();
            }

            if let Some(aria_selected) = element_data.attr(local_name!("aria-selected"))
                && let Ok(aria_selected) = aria_selected.parse()
            {
                builder.set_selected(aria_selected);
            }

            if let Some(aria_label) = element_data.attr(local_name!("aria-label")) {
                builder.set_label(aria_label);
            }
            if let Some(aria_description) = element_data.attr(local_name!("aria-description")) {
                builder.set_description(aria_description);
            }
            if let Some(aria_level) = element_data.attr(local_name!("aria-level"))
                && let Ok(aria_level) = aria_level.parse::<usize>()
                // Accesskit levels are 0-based, while ARIA levels are 1-based
                // See https://docs.rs/accesskit/latest/accesskit/struct.Node.html#method.level
                && let Some(aria_level) = aria_level.checked_sub(1)
            {
                builder.set_level(aria_level);
            } else if let Some(default_level) = default_level_for_tag(&name) {
                builder.set_level(default_level);
            } else if let Some(default_level) = default_level_for_role(role) {
                builder.set_level(default_level);
            }

            self.set_states_and_actions(node.id, element_data, role, &mut builder);

            builder.set_html_tag(name);
        } else if node.is_text_node() {
            builder.set_role(Role::TextRun);
            builder.set_value(node.text_content());
            builder.set_character_lengths(text_character_lengths(&node.text_content()));
            parent.push_labelled_by(id)
        }

        parent.push_child(id);

        (id, builder)
    }
}

fn is_text_input_role(role: Role) -> bool {
    matches!(role, Role::TextInput | Role::MultilineTextInput | Role::SearchInput | Role::PasswordInput | Role::EmailInput | Role::NumberInput | Role::PhoneNumberInput | Role::UrlInput)
}

fn add_input_text_runs(nodes: &mut Vec<(NodeId, AccessKitNode)>) -> Vec<(NodeId, NodeId)> {
    let mut occupied: std::collections::HashSet<_> = nodes.iter().map(|(id, _)| *id).collect();
    occupied.insert(NodeId(u64::MAX));
    let mut next = u64::MAX - 1;
    let mut runs = Vec::new();
    let mut inputs = Vec::new();
    nodes.sort_unstable_by_key(|(id, _)| *id);
    for (input_id, node) in nodes.iter_mut() {
        if !is_text_input_role(node.role()) {
            continue;
        }
        let Some(value) = node.value() else { continue };
        while occupied.contains(&NodeId(next)) {
            next -= 1;
        }
        let id = NodeId(next);
        occupied.insert(id);
        let mut run = AccessKitNode::new(Role::TextRun);
        run.set_value(value);
        run.set_character_lengths(text_character_lengths(value));
        node.push_child(id);
        inputs.push((*input_id, id));
        runs.push((id, run));
    }
    nodes.extend(runs);
    inputs
}

fn text_character_index(value: &str, byte_index: usize) -> usize {
    let mut offset = 0;
    text_character_lengths(value).into_iter().take_while(|length| {
        offset += usize::from(*length);
        offset <= byte_index
    }).count()
}

fn text_character_lengths(value: &str) -> Vec<u8> {
    let mut lengths = Vec::new();
    for grapheme in value.graphemes(true) {
        if let Ok(length) = u8::try_from(grapheme.len()) {
            lengths.push(length);
        } else {
            lengths.extend(grapheme.chars().map(|c| c.len_utf8() as u8));
        }
    }
    lengths
}

#[cfg(test)]
#[path = "accessibility_text_tests.rs"]
mod text_run_tests;

impl BaseDocument {
    /// States (checked, pressed, expanded, selected, disabled, values) and the
    /// actions an assistive technology may request (focus, click, set value,
    /// increment, decrement, expand, collapse), from the element and its ARIA
    /// attributes, per https://www.w3.org/TR/html-aam-1.0/ and
    /// https://www.w3.org/TR/core-aam-1.2/. The platform adapter derives its
    /// patterns from these (on Windows: Invoke, Toggle, SelectionItem,
    /// ExpandCollapse, Value, RangeValue).
    fn set_states_and_actions(
        &self,
        node_id: crate::NodeId,
        element: &ElementData,
        role: Role,
        builder: &mut AccessKitNode,
    ) {
        let tag = &element.name.local;
        let aria = |name: &str| {
            element
                .attrs()
                .iter()
                .find(|a| a.name.ns.is_empty() && &*a.name.local == name)
                .map(|a| a.value.as_str())
        };
        let tristate = |v: &str| match v {
            "true" => Some(Toggled::True),
            "false" => Some(Toggled::False),
            "mixed" => Some(Toggled::Mixed),
            _ => None,
        };

        let is_form_control = *tag == local_name!("button")
            || *tag == local_name!("input")
            || *tag == local_name!("select")
            || *tag == local_name!("textarea");
        let disabled = (is_form_control && element.attr(local_name!("disabled")).is_some())
            || aria("aria-disabled") == Some("true");
        if disabled {
            builder.set_disabled();
        }

        if element.is_focusable_area() && !disabled {
            builder.add_action(Action::Focus);
        }

        // Checked and pressed states.
        let checkable = matches!(
            role,
            Role::CheckBox
                | Role::Switch
                | Role::RadioButton
                | Role::MenuItemCheckBox
                | Role::MenuItemRadio
        );
        if checkable {
            let native = element
                .checkbox_input_checked()
                .map(|c| if c { Toggled::True } else { Toggled::False });
            let toggled = aria("aria-checked")
                .and_then(tristate)
                .or(native)
                .unwrap_or(Toggled::False);
            builder.set_toggled(toggled);
        } else if role == Role::Button {
            if let Some(pressed) = aria("aria-pressed").and_then(tristate) {
                builder.set_toggled(pressed);
            }
        }

        // A drop-down select: its value is the selected option's label; it
        // expands to its picker.
        if crate::is_drop_down(element) {
            let label = self.selected_option(node_id).map(|o| self.option_label(o)).unwrap_or_default();
            builder.set_value(label);
            let open = self.open_select == Some(node_id);
            builder.set_expanded(open);
            if !disabled {
                builder.add_action(if open { Action::Collapse } else { Action::Expand });
            }
        }
        if let Some(expanded) = aria("aria-expanded") {
            let expanded = expanded == "true";
            builder.set_expanded(expanded);
            if !disabled {
                builder.add_action(if expanded { Action::Collapse } else { Action::Expand });
            }
        }
        match aria("aria-haspopup") {
            Some("menu" | "true") => builder.set_has_popup(HasPopup::Menu),
            Some("listbox") => builder.set_has_popup(HasPopup::Listbox),
            Some("tree") => builder.set_has_popup(HasPopup::Tree),
            Some("grid") => builder.set_has_popup(HasPopup::Grid),
            Some("dialog") => builder.set_has_popup(HasPopup::Dialog),
            _ => {}
        }
        if let Some(selected) = aria("aria-selected") {
            builder.set_selected(selected == "true");
        }
        if aria("aria-modal") == Some("true") {
            builder.set_modal();
        }
        if element.attr(local_name!("required")).is_some() || aria("aria-required") == Some("true") {
            builder.set_required();
        }

        // Activation: what a click does for a pointer user.
        let clickable = matches!(
            role,
            Role::Button
                | Role::Link
                | Role::CheckBox
                | Role::Switch
                | Role::RadioButton
                | Role::Tab
                | Role::MenuItem
                | Role::MenuItemCheckBox
                | Role::MenuItemRadio
                | Role::ListBoxOption
                | Role::TreeItem
                | Role::DisclosureTriangle
        ) || (role == Role::ComboBox && aria("aria-expanded").is_some());
        if clickable && !disabled {
            builder.add_action(Action::Click);
        }

        // Text fields: their value, settable unless read-only.
        if let Some(input) = element.text_input_data() {
            let text = input.editor.raw_text();
            if role == Role::PasswordInput {
                builder.set_value("\u{2022}".repeat(text.chars().count()));
            } else {
                builder.set_value(text);
            }
            if let Some(placeholder) = element.attr(local_name!("placeholder")) {
                builder.set_placeholder(placeholder);
            }
            if element.attr(local_name!("readonly")).is_some() || aria("aria-readonly") == Some("true") {
                builder.set_read_only();
            } else if !disabled {
                builder.add_action(Action::SetValue);
            }
        }

        // Ranges: a native range input, or an ARIA range widget.
        if let Some(range) = crate::RangeState::of(element) {
            builder.set_numeric_value(range.value);
            builder.set_min_numeric_value(range.min);
            builder.set_max_numeric_value(range.max);
            builder.set_numeric_value_step(range.small_step());
            builder.set_numeric_value_jump(range.large_step());
            if !disabled {
                builder.add_action(Action::SetValue);
                builder.add_action(Action::Increment);
                builder.add_action(Action::Decrement);
            }
        } else if matches!(
            role,
            Role::Slider | Role::SpinButton | Role::ScrollBar | Role::ProgressIndicator | Role::Meter
        ) {
            let number = |name: &str| aria(name).and_then(|v| v.trim().parse::<f64>().ok());
            if let Some(now) = number("aria-valuenow") {
                builder.set_numeric_value(now);
            }
            if let Some(min) = number("aria-valuemin") {
                builder.set_min_numeric_value(min);
            }
            if let Some(max) = number("aria-valuemax") {
                builder.set_max_numeric_value(max);
            }
            if let Some(text) = aria("aria-valuetext") {
                builder.set_value(text);
            }
            // Stepped with the keys the widget handles (arrows), as browsers
            // do for ARIA range widgets. Their value can't be set directly
            // (the page owns it), so the platform reports it read-only.
            if matches!(role, Role::Slider | Role::SpinButton)
                && !disabled
                && number("aria-valuenow").is_some()
            {
                builder.add_action(Action::Increment);
                builder.add_action(Action::Decrement);
            }
        }
        match aria("aria-orientation") {
            Some("vertical") => builder.set_orientation(accesskit::Orientation::Vertical),
            Some("horizontal") => builder.set_orientation(accesskit::Orientation::Horizontal),
            _ => {}
        }
    }

    /// The element's accessible name: a subset of
    /// https://www.w3.org/TR/accname-1.2/ in priority order (`aria-labelledby`,
    /// `aria-label`, native labels such as `<label>` and `alt`, name from
    /// content for the roles that allow it, then `title` and `placeholder`).
    fn accessible_name(
        &self,
        node: &BlitzDomNode,
        element: &ElementData,
        role: Role,
    ) -> Option<String> {
        let non_empty = |s: String| {
            let s = collapse_whitespace(&s);
            (!s.is_empty()).then_some(s)
        };

        if let Some(ids) = element.attr(local_name!("aria-labelledby")) {
            let text: Vec<String> = ids
                .split_ascii_whitespace()
                .filter_map(|id| self.get_element_by_id(id))
                .map(|id| self.nodes[id].text_content())
                .collect();
            if let Some(name) = non_empty(text.join(" ")) {
                return Some(name);
            }
        }
        if let Some(name) = element.attr(local_name!("aria-label")).map(str::to_owned).and_then(non_empty) {
            return Some(name);
        }

        let tag = &element.name.local;
        let is_labelable = *tag == local_name!("input")
            || *tag == local_name!("textarea")
            || *tag == local_name!("select")
            || *tag == local_name!("button")
            || *tag == local_name!("meter")
            || *tag == local_name!("progress");
        if is_labelable && element.attr(local_name!("type")) != Some("hidden") {
            if let Some(name) = self.label_text(node, element).and_then(non_empty) {
                return Some(name);
            }
        }
        if *tag == local_name!("img") || element.attr(local_name!("type")) == Some("image") {
            if let Some(alt) = element.attr(local_name!("alt")) {
                return non_empty(alt.to_owned());
            }
        }
        if name_from_content(role) {
            if let Some(name) = non_empty(self.text_for_name(node)) {
                return Some(name);
            }
        }
        element
            .attr(local_name!("title"))
            .or_else(|| element.attr(local_name!("placeholder")))
            .map(str::to_owned)
            .and_then(non_empty)
    }

    /// A node's text for name computation: descendant text, with a space
    /// around the content of children that aren't inline (accname 2F: block
    /// and flex children read as separate words, e.g. "CI-065 14.8").
    fn text_for_name(&self, node: &BlitzDomNode) -> String {
        let mut out = String::new();
        for &child_id in node.children.iter() {
            let child = &self.nodes[child_id];
            if child.is_text_node() {
                out.push_str(&child.text_content());
            } else if child.element_data().is_some() && !child.is_hidden_from_accessibility_tree() {
                let inline = child
                    .display_style()
                    .is_some_and(|d| d.outside() == style::values::specified::box_::DisplayOutside::Inline
                        && d.inside() == style::values::specified::box_::DisplayInside::Flow);
                let text = self.text_for_name(child);
                if inline {
                    out.push_str(&text);
                } else {
                    out.push(' ');
                    out.push_str(&text);
                    out.push(' ');
                }
            }
        }
        out
    }

    /// Text of the `<label>` elements associated with a labelable element:
    /// those whose `for` names its id, and an ancestor `<label>`.
    fn label_text(&self, node: &BlitzDomNode, element: &ElementData) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(id) = element.attr(local_name!("id")).filter(|id| !id.is_empty()) {
            for (_, candidate) in self.nodes.iter() {
                if let Some(el) = candidate.element_data() {
                    if el.name.local == local_name!("label") && el.attr(local_name!("for")) == Some(id) {
                        parts.push(self.text_for_name(candidate));
                    }
                }
            }
        }
        let mut ancestor = node.parent;
        while let Some(id) = ancestor {
            let candidate = &self.nodes[id];
            if candidate.element_data().is_some_and(|el| el.name.local == local_name!("label")) {
                parts.push(self.text_for_name(candidate));
                break;
            }
            ancestor = candidate.parent;
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// Roles whose name may come from their content (accname, "name from content").
fn name_from_content(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::Cell
            | Role::CheckBox
            | Role::ColumnHeader
            | Role::GridCell
            | Role::Heading
            | Role::Link
            | Role::ListBoxOption
            | Role::MenuItem
            | Role::MenuItemCheckBox
            | Role::MenuItemRadio
            | Role::RadioButton
            | Role::Row
            | Role::RowHeader
            | Role::Switch
            | Role::Tab
            | Role::Tooltip
            | Role::TreeItem
    )
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl BlitzDomNode {
    // https://www.w3.org/TR/wai-aria-1.2/#tree_exclusion
    fn is_hidden_from_accessibility_tree(&self) -> bool {
        self.try_stylo_element_data()
            .as_ref()
            .and_then(|s| s.get())
            // A node created since the last style pass has no style yet; the tree
            // is rebuilt once it has one.
            .and_then(|s| s.styles.get_primary().cloned())
            .map(|primary| {
                primary.get_box().clone_display().is_none()
                    || primary.clone_visibility() == visibility::computed_value::T::Hidden
            })
            .unwrap_or(false)
    }

    fn write_text_alternative(&self, out: &mut FlatText) {
        match self
            .attr(local_name!("aria-label"))
            .filter(|aria_label| !aria_label.trim_ascii().is_empty())
        {
            Some(aria_label) => out.push(aria_label),
            None => self.write_flat_text_content(out),
        }
    }

    fn write_flat_text_content(&self, out: &mut FlatText) {
        match &self.data {
            NodeData::Text(data) => out.push(&data.content),
            NodeData::Element(..) | NodeData::AnonymousBlock(..) => {
                for &child_id in &self.children {
                    self.with(child_id).write_flat_text_content(out);
                }
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct FlatText {
    text: String,
    pending_space: bool,
}

impl FlatText {
    fn push(&mut self, text: &str) {
        for (i, word) in text.split(|c: char| c.is_ascii_whitespace()).enumerate() {
            self.pending_space |= i > 0;
            if word.is_empty() {
                continue;
            }
            if self.pending_space && !self.text.is_empty() {
                self.text.push(' ');
            }
            self.pending_space = false;
            self.text.push_str(word);
        }
    }

    fn break_word(&mut self) {
        self.pending_space = true;
    }

    fn clear(&mut self) {
        self.text.clear();
        self.pending_space = false;
    }
}

fn role_from_name(name: &str) -> Option<Role> {
    match name {
        "alert" => Some(Role::Alert),
        "alertdialog" => Some(Role::AlertDialog),
        "button" => Some(Role::Button),
        "checkbox" => Some(Role::CheckBox),
        "dialog" => Some(Role::Dialog),
        "gridcell" => Some(Role::GridCell),
        "link" => Some(Role::Link),
        "log" => Some(Role::Log),
        "marquee" => Some(Role::Marquee),
        "menuitem" => Some(Role::MenuItem),
        "menuitemcheckbox" => Some(Role::MenuItemCheckBox),
        "menuitemradio" => Some(Role::MenuItemRadio),
        "option" => Some(Role::ListBoxOption),
        "progressbar" => Some(Role::ProgressIndicator),
        "radio" => Some(Role::RadioButton),
        "scrollbar" => Some(Role::ScrollBar),
        "slider" => Some(Role::Slider),
        "switch" => Some(Role::Switch),
        "searchbox" => Some(Role::SearchInput),
        "separator" => Some(Role::Splitter),
        "meter" => Some(Role::Meter),
        "none" | "presentation" => Some(Role::GenericContainer),
        "spinbutton" => Some(Role::SpinButton),
        "status" => Some(Role::Status),
        "tab" => Some(Role::Tab),
        "tabpanel" => Some(Role::TabPanel),
        "textbox" => Some(Role::TextInput),
        "timer" => Some(Role::Timer),
        "tooltip" => Some(Role::Tooltip),
        "treeitem" => Some(Role::TreeItem),
        "combobox" => Some(Role::ComboBox),
        "grid" => Some(Role::Grid),
        "listbox" => Some(Role::ListBox),
        "menu" => Some(Role::Menu),
        "menubar" => Some(Role::MenuBar),
        "radiogroup" => Some(Role::RadioGroup),
        "tablist" => Some(Role::TabList),
        "tree" => Some(Role::Tree),
        "treegrid" => Some(Role::TreeGrid),
        "article" => Some(Role::Article),
        "columnheader" => Some(Role::ColumnHeader),
        "definition" => Some(Role::Definition),
        "document" => Some(Role::Document),
        "group" => Some(Role::Group),
        "heading" => Some(Role::Heading),
        "img" => Some(Role::Image),
        "list" => Some(Role::List),
        "listitem" => Some(Role::ListItem),
        "math" => Some(Role::Math),
        "note" => Some(Role::Note),
        "region" => Some(Role::Region),
        "row" => Some(Role::Row),
        "rowgroup" => Some(Role::RowGroup),
        "rowheader" => Some(Role::RowHeader),
        "toolbar" => Some(Role::Toolbar),
        "application" => Some(Role::Application),
        "banner" => Some(Role::Banner),
        "complementary" => Some(Role::Complementary),
        "contentinfo" => Some(Role::ContentInfo),
        "form" => Some(Role::Form),
        "main" => Some(Role::Main),
        "navigation" => Some(Role::Navigation),
        "search" => Some(Role::Search),
        _ => None,
    }
}

fn default_level_for_tag(name: &str) -> Option<usize> {
    // Accesskit levels are 0-based, while ARIA levels are 1-based
    // See https://docs.rs/accesskit/latest/accesskit/struct.Node.html#method.level
    match name {
        "h1" => Some(0),
        "h2" => Some(1),
        "h3" => Some(2),
        "h4" => Some(3),
        "h5" => Some(4),
        "h6" => Some(5),
        _ => None,
    }
}

fn default_level_for_role(role: Role) -> Option<usize> {
    match role {
        // ARIA specifies an implicit level of 2 for the heading role. Accesskit levels are 0-based,
        // so this turns into level 1
        Role::Heading => Some(1),
        _ => None,
    }
}

fn role_from_element_data(element_data: &ElementData) -> Option<Role> {
    // <https://www.w3.org/TR/html-aam-1.0/>
    match &*element_data.name.local {
        // Document structure
        "article" => Some(Role::Article),
        "aside" => Some(Role::Complementary),
        "footer" => Some(Role::Footer),
        "header" => Some(Role::Header),
        "main" => Some(Role::Main),
        "nav" => Some(Role::Navigation),
        "search" => Some(Role::Search),
        "section" => Some(Role::Section),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(Role::Heading),
        "p" => Some(Role::Paragraph),
        "blockquote" => Some(Role::Blockquote),
        "figure" => Some(Role::Figure),
        "figcaption" | "caption" => Some(Role::Caption),
        "hr" => Some(Role::Splitter),

        // Grouping
        "ul" | "ol" | "menu" => Some(Role::List),
        "li" => Some(Role::ListItem),
        "dl" => Some(Role::DescriptionList),
        "dt" => Some(Role::Term),
        "dd" => Some(Role::Definition),
        "dialog" => Some(Role::Dialog),
        "fieldset" => Some(Role::Group),
        "form" => Some(Role::Form),
        "div" => Some(Role::GenericContainer),

        // Tables
        "table" => Some(Role::Table),
        "thead" | "tbody" | "tfoot" => Some(Role::RowGroup),
        "tr" => Some(Role::Row),
        "td" => Some(Role::Cell),
        "th" => match element_data.attr(local_name!("scope")) {
            Some("row") | Some("rowgroup") => Some(Role::RowHeader),
            _ => Some(Role::ColumnHeader),
        },

        // Interactive
        // An <a> is only a link when it has an href.
        "a" => match element_data.attr(local_name!("href")) {
            Some(_) => Some(Role::Link),
            None => Some(Role::GenericContainer),
        },
        "button" => Some(Role::Button),
        "label" => Some(Role::Label),
        "legend" => Some(Role::Label),
        "select" => match element_data.attr(local_name!("multiple")) {
            Some(_) => Some(Role::ListBox),
            None => Some(Role::ComboBox),
        },
        "option" => Some(Role::ListBoxOption),
        "textarea" => Some(Role::MultilineTextInput),
        "progress" => Some(Role::ProgressIndicator),
        "meter" => Some(Role::Meter),
        "output" => Some(Role::Status),
        "summary" => Some(Role::DisclosureTriangle),

        // Inline semantics
        "code" => Some(Role::Code),
        "em" => Some(Role::Emphasis),
        "strong" => Some(Role::Strong),
        "mark" => Some(Role::Mark),
        "time" => Some(Role::Time),
        "img" => Some(Role::Image),
        "iframe" => Some(Role::Iframe),

        "input" => {
            let ty = element_data.attr(local_name!("type")).unwrap_or("text");
            match ty {
                "button" | "submit" | "reset" => Some(Role::Button),
                "checkbox" => Some(Role::CheckBox),
                "color" => Some(Role::ColorWell),
                "date" => Some(Role::DateInput),
                "datetime-local" => Some(Role::DateTimeInput),
                "email" => Some(Role::EmailInput),
                "number" => Some(Role::NumberInput),
                "password" => Some(Role::PasswordInput),
                "radio" => Some(Role::RadioButton),
                "range" => Some(Role::Slider),
                "search" => Some(Role::SearchInput),
                "tel" => Some(Role::PhoneNumberInput),
                "time" => Some(Role::TimeInput),
                _ => Some(Role::TextInput),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{Attribute, BaseDocument, DocumentConfig, EventDriver, NoopEventHandler, QualName, qual_name};
    use accesskit::{Action, ActionData, ActionRequest, Node, NodeId as AxId, Toggled, TreeId};
    use blitz_traits::node_id::NodeId;
    use blitz_traits::shell::{ColorScheme, Viewport};

    fn doc_with(elements: &[(&str, &[(&str, &str)])]) -> (BaseDocument, Vec<NodeId>) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html", html), vec![]);
        let body = m.create_element(qual_name!("body", html), vec![]);
        let mut ids = Vec::new();
        for (tag, attrs) in elements {
            let attrs = attrs
                .iter()
                .map(|(k, v)| Attribute {
                    name: QualName::new(None, markup5ever::ns!(), markup5ever::LocalName::from(*k)),
                    value: v.to_string(),
                })
                .collect();
            let name = QualName::new(None, markup5ever::ns!(html), markup5ever::LocalName::from(*tag));
            ids.push(m.create_element(name, attrs));
        }
        m.append_children(body, &ids);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);
        (doc, ids)
    }

    fn ax_node(doc: &BaseDocument, id: NodeId) -> Node {
        let tree = doc.build_accessibility_tree();
        tree.nodes.into_iter().find(|(n, _)| n.0 == id.as_u64()).expect("node in the tree").1
    }

    #[test]
    fn document_window_bounds_follow_viewport_size_scale_and_zoom() {
        let (mut doc, _) = doc_with(&[("button", &[])]);
        for (width, height, hidpi, zoom) in [
            (400, 300, 1.0, 1.0),
            (900, 600, 1.5, 1.25),
            (1120, 820, 2.0, 1.0),
            (0, 0, 1.0, 1.0),
        ] {
            let mut viewport = Viewport::new(width, height, hidpi, ColorScheme::Light);
            viewport.zoom = zoom;
            let scale = viewport.scale_f64();
            doc.set_viewport(viewport);
            doc.resolve(0.0);
            let document = ax_node(&doc, doc.root_node().id);
            assert_eq!(document.role(), accesskit::Role::Window);
            let bounds = document
                .bounds()
                .expect("document Window has viewport bounds");
            assert_eq!(
                bounds,
                accesskit::Rect::new(0.0, 0.0, width as f64 / scale, height as f64 / scale)
            );
            let tree = doc.build_accessibility_tree();
            let window = &tree
                .nodes
                .iter()
                .find(|(id, _)| id.0 == u64::MAX)
                .unwrap()
                .1;
            assert_eq!(
                window.transform().unwrap().transform_rect_bbox(bounds),
                accesskit::Rect::new(0.0, 0.0, width as f64, height as f64)
            );
        }
    }

    fn act(doc: &mut BaseDocument, id: NodeId, action: Action, data: Option<ActionData>) -> bool {
        let request = ActionRequest { action, target_tree: TreeId::ROOT, target_node: AxId(id.as_u64()), data };
        EventDriver::new(doc, NoopEventHandler).handle_accessibility_action(&request)
    }

    /// Controls expose their state and the actions an assistive technology
    /// can request, from the element and its ARIA attributes.
    #[test]
    fn states_and_actions() {
        let (doc, ids) = doc_with(&[
            ("input", &[("type", "checkbox"), ("checked", "")]),
            ("button", &[("role", "switch"), ("aria-checked", "false")]),
            ("button", &[("aria-pressed", "true")]),
            ("button", &[("aria-expanded", "false"), ("aria-haspopup", "dialog")]),
            ("input", &[("type", "text"), ("value", "abc")]),
            ("div", &[("role", "slider"), ("tabindex", "0"), ("aria-valuenow", "5"), ("aria-valuemin", "0"), ("aria-valuemax", "10")]),
            ("input", &[("type", "range"), ("min", "0"), ("max", "4"), ("value", "1")]),
            ("button", &[("disabled", "")]),
        ]);
        let n: Vec<Node> = ids.iter().map(|&id| ax_node(&doc, id)).collect();

        assert_eq!(n[0].toggled(), Some(Toggled::True), "native checkbox");
        assert!(n[0].supports_action(Action::Click) && n[0].supports_action(Action::Focus));
        assert_eq!(n[1].toggled(), Some(Toggled::False), "ARIA switch");
        assert_eq!(n[2].toggled(), Some(Toggled::True), "toggle button (aria-pressed)");
        assert_eq!(n[3].is_expanded(), Some(false));
        assert!(n[3].supports_action(Action::Expand) && !n[3].supports_action(Action::Collapse));
        assert_eq!(n[4].value(), Some("abc"));
        assert!(n[4].supports_action(Action::SetValue), "editable text field");
        assert_eq!((n[5].numeric_value(), n[5].min_numeric_value(), n[5].max_numeric_value()), (Some(5.0), Some(0.0), Some(10.0)));
        assert!(n[5].supports_action(Action::Increment) && !n[5].supports_action(Action::SetValue), "ARIA slider: stepped, not set");
        assert_eq!(n[6].numeric_value(), Some(1.0), "native range");
        assert!(n[6].supports_action(Action::SetValue));
        assert!(n[7].is_disabled() && !n[7].supports_action(Action::Click) && !n[7].supports_action(Action::Focus));
    }

    #[test]
    fn set_value_respects_text_mutation_restrictions() {
        for restriction in ["readonly", "aria-readonly", "disabled"] {
            let (mut doc, ids) = doc_with(&[
                ("input", &[("type", "text"), ("value", "original"), (restriction, "true")]),
            ]);
            assert!(!ax_node(&doc, ids[0]).supports_action(Action::SetValue));
            assert!(!act(&mut doc, ids[0], Action::SetValue, Some(ActionData::Value("changed".into()))), "{restriction} rejects SetValue");
            let input = doc.nodes[ids[0]].element_data().unwrap().text_input_data().unwrap();
            assert_eq!(input.editor.raw_text(), "original", "{restriction} preserves editor value");
            assert_eq!(ax_node(&doc, ids[0]).value(), Some("original"));
        }
    }

    #[test]
    fn password_set_value_keeps_accessibility_tree_masked() {
        let (mut doc, ids) = doc_with(&[("input", &[("type", "password")])]);
        assert!(act(&mut doc, ids[0], Action::SetValue, Some(ActionData::Value("hunter2".into()))));
        let input = doc.nodes[ids[0]].element_data().unwrap().text_input_data().unwrap();
        assert_eq!(input.value(), "hunter2");
        assert_eq!(input.editor.raw_text(), "•••••••");
        let tree = doc.build_accessibility_tree();
        let node = &tree.nodes.iter().find(|(id, _)| id.0 == ids[0].as_u64()).unwrap().1;
        assert_eq!(node.value(), Some("•••••••"));
        assert_eq!(node.children().len(), 1);
        let run = &tree.nodes.iter().find(|(id, _)| *id == node.children()[0]).unwrap().1;
        assert_eq!(run.role(), accesskit::Role::TextRun);
        assert_eq!(run.value(), Some("•••••••"));
        assert!(tree.nodes.iter().all(|(_, node)| !format!("{node:?}").contains("hunter2")));
    }

    /// Requested actions do what the input they stand for does.
    #[test]
    fn actions_do_what_input_does() {
        let (mut doc, ids) = doc_with(&[
            ("input", &[("type", "checkbox")]),
            ("input", &[("type", "text"), ("value", "abc")]),
            ("input", &[("type", "range"), ("min", "0"), ("max", "4"), ("step", "0.5"), ("value", "1")]),
            ("button", &[]),
            ("button", &[("disabled", "")]),
        ]);
        assert!(act(&mut doc, ids[0], Action::Click, None));
        assert_eq!(doc.nodes[ids[0]].element_data().unwrap().checkbox_input_checked(), Some(true), "a click checks it");

        assert!(act(&mut doc, ids[1], Action::SetValue, Some(ActionData::Value("xyz".into()))));
        let text = doc.nodes[ids[1]].element_data().unwrap().text_input_data().unwrap().editor.raw_text().to_owned();
        assert_eq!(text, "xyz");

        assert!(act(&mut doc, ids[2], Action::Increment, None));
        assert_eq!(ax_node(&doc, ids[2]).numeric_value(), Some(1.5), "one step up");
        assert!(act(&mut doc, ids[2], Action::SetValue, Some(ActionData::NumericValue(9.0))));
        assert_eq!(ax_node(&doc, ids[2]).numeric_value(), Some(4.0), "clamped to max");

        assert!(act(&mut doc, ids[3], Action::Focus, None));
        assert_eq!(doc.get_focussed_node_id(), Some(ids[3]));
        assert!(!act(&mut doc, ids[4], Action::Click, None), "disabled controls take no actions");
    }
}
