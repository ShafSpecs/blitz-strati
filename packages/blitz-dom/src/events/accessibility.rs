//! Actions requested by an assistive technology (AccessKit), carried out as
//! the user input they stand for, so the page sees the same events: focus and
//! blur; a press (pointer and mouse down and up, then click) for activation,
//! as Chromium sends, since widgets often act on pointerdown; input; and
//! keydown/keyup for ARIA range widgets.

use super::driver::{EventDriver, EventHandler};
use super::focus::generate_focus_events;
use crate::{BaseDocument, RangeState, local_name};
use accesskit::{Action, ActionData, ActionRequest};
use blitz_traits::events::{
    BlitzInputEvent, BlitzKeyEvent, DomEvent, DomEventData, KeyState, MouseEventButton,
    MouseEventButtons,
};
use blitz_traits::node_id::NodeId;
use keyboard_types::{Code, Key, Location, Modifiers, NamedKey};

impl<Handler: EventHandler> EventDriver<'_, Handler> {
    /// Carries out an action an assistive technology requested. Returns
    /// whether the target exists and the action applied to it.
    pub fn handle_accessibility_action(&mut self, request: &ActionRequest) -> bool {
        let target = NodeId::from_u64(request.target_node.0);
        if self.doc.inner().get_node(target).is_none() {
            return false;
        }
        let handled = match request.action {
            Action::Focus => self.ax_focus(Some(target)),
            Action::Blur => self.ax_focus(None),
            // Expanding and collapsing is what activating the control does
            // (the adapter only asks for the state it doesn't have).
            Action::Click | Action::Expand | Action::Collapse => self.ax_click(target),
            Action::SetValue => match &request.data {
                Some(ActionData::Value(text)) => self.ax_set_text(target, text),
                Some(ActionData::NumericValue(value)) => self.ax_set_range(target, |_| *value),
                _ => false,
            },
            Action::Increment => self.ax_step(target, true),
            Action::Decrement => self.ax_step(target, false),
            _ => false,
        };
        self.process_queue();
        handled
    }

    /// Moves focus (or removes it), with focus and blur events. Focus moved
    /// by an assistive technology is shown, as keyboard focus is.
    fn ax_focus(&mut self, target: Option<NodeId>) -> bool {
        let mut doc = self.doc.inner_mut();
        if let Some(id) = target {
            // Any focusable area, as `element.focus()` allows (a roving
            // tabindex leaves most items at -1).
            if !doc.nodes[id].element_data().is_some_and(|el| el.is_focusable_area()) {
                return false;
            }
            doc.set_keyboard_modality(true);
        }
        let queue = &mut self.queue;
        generate_focus_events(
            &mut doc,
            &mut |doc| match target {
                Some(id) => {
                    doc.set_focus_to(id);
                }
                None => doc.clear_focus(),
            },
            &mut |event| queue.push_back(event),
        );
        true
    }

    /// Activates the element as a primary-button press at its centre does
    /// (focus on press, then the click the release produces), aimed at the
    /// element itself rather than whatever is drawn over it.
    fn ax_click(&mut self, target: NodeId) -> bool {
        // An option of a drop-down select (shown in its picker, or not): it's
        // chosen, as a click on it in the picker would.
        let option_of = {
            let doc = self.doc.inner();
            doc.nodes[target]
                .element_data()
                .is_some_and(|e| e.name.local == local_name!("option"))
                .then(|| doc.drop_down_select_of(target))
                .flatten()
        };
        if let Some(select) = option_of {
            let mut doc = self.doc.inner_mut();
            if doc.option_disabled(target) {
                return false;
            }
            doc.close_drop_down();
            let queue = &mut self.queue;
            doc.choose_option(select, target, &mut |e| queue.push_back(e));
            return true;
        }
        let down = {
            let doc = self.doc.inner();
            if is_disabled(&doc, target) {
                return false;
            }
            let mut down = doc.nodes[target].synthetic_click_event_data(Modifiers::empty());
            down.button = MouseEventButton::Main;
            down.buttons = MouseEventButtons::Primary;
            down.details.pressure = 0.5;
            down
        };
        let mut up = down.clone();
        up.buttons = MouseEventButtons::None;
        up.details.pressure = 0.0;
        self.doc.inner_mut().set_mousedown_node_id(Some(target));
        self.handle_pointer_event(
            target,
            down,
            DomEventData::PointerDown,
            Some(DomEventData::MouseDown),
            DomEventData::TouchStart,
        );
        if self.doc.inner().get_node(target).is_some() {
            self.handle_pointer_event(
                target,
                up,
                DomEventData::PointerUp,
                Some(DomEventData::MouseUp),
                DomEventData::TouchEnd,
            );
        }
        self.doc.inner_mut().set_pointer_capture(None);
        true
    }

    /// Replaces a text field's value (one undo step), then fires `input`.
    fn ax_set_text(&mut self, target: NodeId, text: &str) -> bool {
        let mut doc = self.doc.inner_mut();
        if is_disabled(&doc, target) {
            return false;
        }
        let doc = &mut *doc;
        let Some(el) = doc.nodes[target].element_data_mut() else {
            return false;
        };
        if el.attr(local_name!("readonly")).is_some() {
            return false;
        }
        let Some(input) = el.text_input_data_mut() else {
            return false;
        };
        if input.replace_all(&mut doc.font_ctx.lock().unwrap(), &mut doc.layout_ctx, text) {
            let value = input.value().to_string();
            doc.shell_provider.request_redraw();
            self.queue
                .push_back(DomEvent::new(target, DomEventData::Input(BlitzInputEvent { value })));
        }
        true
    }

    /// Sets a native range input's value (`input` fires if it changed).
    fn ax_set_range(&mut self, target: NodeId, value: impl FnOnce(&RangeState) -> f64) -> bool {
        let mut doc = self.doc.inner_mut();
        if is_disabled(&doc, target) {
            return false;
        }
        let Some(range) = doc.nodes[target].element_data().and_then(RangeState::of) else {
            return false;
        };
        let queue = &mut self.queue;
        doc.set_range_value(target, value(&range), &mut |event| queue.push_back(event));
        true
    }

    /// One step up or down: a native range input steps its value; an ARIA
    /// range widget gets focus and an arrow key, as browsers do.
    fn ax_step(&mut self, target: NodeId, up: bool) -> bool {
        let native = self.doc.inner().nodes[target]
            .element_data()
            .is_some_and(|el| RangeState::of(el).is_some());
        if native {
            return self.ax_set_range(target, |r| {
                if up { r.value + r.small_step() } else { r.value - r.small_step() }
            });
        }
        if is_disabled(&self.doc.inner(), target) {
            return false;
        }
        self.ax_focus(Some(target));
        self.process_queue();
        let (key, code) = if up { (Key::Named(NamedKey::ArrowUp), Code::ArrowUp) } else { (Key::Named(NamedKey::ArrowDown), Code::ArrowDown) };
        for state in [KeyState::Pressed, KeyState::Released] {
            let event = BlitzKeyEvent {
                key: key.clone(),
                code,
                modifiers: Modifiers::empty(),
                location: Location::Standard,
                is_auto_repeating: false,
                is_composing: false,
                state,
                text: None,
            };
            let data = match state {
                KeyState::Pressed => DomEventData::KeyDown(event),
                KeyState::Released => DomEventData::KeyUp(event),
            };
            self.queue.push_back(DomEvent::new(target, data));
        }
        true
    }
}

/// Disabled form controls (the attribute; a disabled ancestor fieldset isn't
/// considered yet) and `aria-disabled="true"` elements take no actions.
fn is_disabled(doc: &BaseDocument, id: NodeId) -> bool {
    let Some(el) = doc.nodes[id].element_data() else {
        return false;
    };
    let form_control = matches!(&*el.name.local, "button" | "input" | "select" | "textarea");
    (form_control && el.attr(local_name!("disabled")).is_some())
        || el
            .attrs()
            .iter()
            .any(|a| &*a.name.local == "aria-disabled" && a.value == "true")
}
