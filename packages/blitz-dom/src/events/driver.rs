use crate::Document;
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, DomEvent, DomEventData, EventState, Point, PointerCoords,
    UiEvent,
};
use blitz_traits::node_id::NodeId;
use keyboard_types::{Key, Modifiers, NamedKey};
use std::collections::VecDeque;

pub trait EventHandler {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        doc: &mut dyn Document,
        event_state: &mut EventState,
    );
}

pub struct NoopEventHandler;
impl EventHandler for NoopEventHandler {
    fn handle_event(
        &mut self,
        _chain: &[NodeId],
        _event: &mut DomEvent,
        _doc: &mut dyn Document,
        _event_state: &mut EventState,
    ) {
        // Do nothing
    }
}

pub struct EventDriver<'doc, Handler: EventHandler> {
    pub(super) doc: &'doc mut dyn Document,
    handler: Handler,
    pub(super) queue: VecDeque<DomEvent>,
}

impl<'doc, Handler: EventHandler> EventDriver<'doc, Handler> {
    pub fn new(doc: &'doc mut dyn Document, handler: Handler) -> Self {
        EventDriver {
            doc,
            handler,
            queue: VecDeque::with_capacity(4),
        }
    }

    pub fn handle_pointer_move(&mut self, event: &BlitzPointerEvent) -> Option<NodeId> {
        let mut doc = self.doc.inner_mut();

        let prev_hover_node_id = doc.hover_node_id;
        let changed = doc.set_hover_to(event.page_x(), event.page_y());
        let hover_node_id = doc.hover_node_id;

        drop(doc);

        if !changed {
            return prev_hover_node_id;
        }

        self.dispatch_hover_change(prev_hover_node_id, hover_node_id, event);

        hover_node_id
    }

    /// End hover when the pointer leaves the document, dispatching out/leave
    /// events to the previously hovered chain
    pub fn handle_pointer_leave(&mut self, event: &BlitzPointerEvent) {
        let mut doc = self.doc.inner_mut();
        let prev_hover_node_id = doc.hover_node_id;
        doc.clear_hover();
        drop(doc);

        self.dispatch_hover_change(prev_hover_node_id, None, event);
    }

    fn dispatch_hover_change(
        &mut self,
        prev_hover_node_id: Option<NodeId>,
        hover_node_id: Option<NodeId>,
        event: &BlitzPointerEvent,
    ) {
        let doc = self.doc.inner();
        let mut old_chain = prev_hover_node_id
            .map(|id| doc.node_chain(id))
            .unwrap_or_default();
        let mut new_chain = hover_node_id
            .map(|id| doc.node_chain(id))
            .unwrap_or_default();
        old_chain.reverse();
        new_chain.reverse();

        // Find the difference in the node chain of the last hovered objected and the newest
        let old_len = old_chain.len();
        let new_len = new_chain.len();

        let first_difference_index = old_chain
            .iter()
            .zip(&new_chain)
            .position(|(old, new)| old != new)
            .unwrap_or_else(|| old_len.min(new_len));

        drop(doc);

        let is_mouse = event.is_mouse();

        if let Some(target) = prev_hover_node_id {
            self.handle_dom_event(DomEvent::new(
                target,
                DomEventData::PointerOut(event.clone()),
            ));
            if is_mouse {
                self.handle_dom_event(DomEvent::new(target, DomEventData::MouseOut(event.clone())));
            }

            // Send an mouseleave event to all old elements on the chain
            for node_id in old_chain
                .get(first_difference_index..)
                .unwrap_or(&[])
                .iter()
            {
                self.handle_dom_event(DomEvent::new(
                    *node_id,
                    DomEventData::PointerLeave(event.clone()),
                ));
                if is_mouse {
                    self.handle_dom_event(DomEvent::new(
                        *node_id,
                        DomEventData::MouseLeave(event.clone()),
                    ));
                }
            }
        }

        if let Some(target) = hover_node_id {
            self.handle_dom_event(DomEvent::new(
                target,
                DomEventData::PointerOver(event.clone()),
            ));

            if is_mouse {
                self.handle_dom_event(DomEvent::new(
                    target,
                    DomEventData::MouseOver(event.clone()),
                ));
            }

            // Send an mouseenter event to all new elements on the chain
            for node_id in new_chain
                .get(first_difference_index..)
                .unwrap_or(&[])
                .iter()
            {
                self.handle_dom_event(DomEvent::new(
                    *node_id,
                    DomEventData::PointerEnter(event.clone()),
                ));

                if is_mouse {
                    self.handle_dom_event(DomEvent::new(
                        *node_id,
                        DomEventData::MouseEnter(event.clone()),
                    ));
                }
            }
        }
    }

    pub fn handle_ui_event(&mut self, event: UiEvent) {
        // An open select's picker takes the pointer and the keyboard, as the
        // separate popup a browser shows would.
        if self.doc.inner().open_select.is_some() && self.handle_picker_event(&event) {
            self.process_queue();
            self.close_picker_without_focus();
            return;
        }
        self.handle_ui_event_inner(event);
        self.close_picker_without_focus();
    }

    /// A picker stays open only while its select has focus.
    fn close_picker_without_focus(&mut self) {
        let mut doc = self.doc.inner_mut();
        if let Some(select) = doc.open_select
            && doc.focus_node_id != Some(select)
        {
            doc.close_drop_down();
        }
    }

    /// Input while a select's picker is open. Returns whether the picker used
    /// it (the page doesn't see it).
    fn handle_picker_event(&mut self, event: &UiEvent) -> bool {
        let Self { doc, queue, .. } = self;
        let mut doc = doc.inner_mut();
        let mut dispatch = |e: DomEvent| queue.push_back(e);
        let at = |c: &PointerCoords| (c.client_x as f64, c.client_y as f64);
        match event {
            UiEvent::PointerMove(e) => {
                let (x, y) = at(&e.coords);
                doc.picker_hover(x, y)
            }
            UiEvent::PointerDown(e) => {
                let (x, y) = at(&e.coords);
                if !doc
                    .open_picker()
                    .is_some_and(|(_, p)| p.rect.contains(kurbo::Point::new(x, y)))
                {
                    // A press elsewhere closes the picker; a press on the
                    // select itself only that.
                    doc.close_drop_down();
                }
                true
            }
            UiEvent::PointerUp(e) => {
                let (x, y) = at(&e.coords);
                doc.picker_release(x, y, &mut dispatch);
                doc.unactive_node();
                doc.set_pointer_capture(None);
                true
            }
            UiEvent::Wheel(e) => {
                let (x, y) = at(&e.coords);
                let dy = match e.delta {
                    blitz_traits::events::BlitzWheelDelta::Lines(_, y) => -y * 20.0,
                    blitz_traits::events::BlitzWheelDelta::Pixels(_, y) => -y,
                };
                if !doc.picker_scroll(x, y, dy) {
                    doc.close_drop_down();
                    return false;
                }
                true
            }
            UiEvent::KeyDown(e) => {
                doc.picker_key(&e.key, e.modifiers.contains(Modifiers::ALT), &mut dispatch)
            }
            UiEvent::KeyUp(_) => true,
            _ => false,
        }
    }

    fn handle_ui_event_inner(&mut self, event: UiEvent) {
        let doc = self.doc.inner();

        // A non-hovering pointer (finger) ends hover once it is lifted or cancelled
        let mut hover_end_event = None;
        let mut hover_node_id = doc.hover_node_id;
        let focussed_node_id = doc.focus_node_id;
        let prev_hover = doc.hover_state();
        drop(doc);

        // A document the pointer has left has no say over the cursor
        let is_leave = matches!(event, UiEvent::PointerLeave(_));
        let is_release = matches!(event, UiEvent::PointerUp(_) | UiEvent::PointerCancel(_));

        // Update document input state (hover, focus, active, etc)
        match &event {
            UiEvent::PointerMove(event) => {
                hover_node_id = self.handle_pointer_move(event);
            }
            UiEvent::PointerDown(event) => {
                hover_node_id = self.handle_pointer_move(event);
                let mut doc = self.doc.inner_mut();
                doc.set_keyboard_modality(false);
                doc.active_node();
                doc.set_mousedown_node_id(hover_node_id);
            }
            // A key that isn't a modifier or a shortcut is keyboard use (`:focus-visible`)
            UiEvent::KeyDown(event)
                if !is_modifier_key(&event.key)
                    && !event
                        .modifiers
                        .intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) =>
            {
                self.doc.inner_mut().set_keyboard_modality(true);
            }
            UiEvent::PointerUp(event) => {
                hover_node_id = self.handle_pointer_move(event);
                let mut doc = self.doc.inner_mut();
                doc.unactive_node();

                if event.is_primary && matches!(event.id, BlitzPointerId::Finger(_)) {
                    hover_end_event = Some(event.clone());
                }
            }
            UiEvent::PointerCancel(event) => {
                hover_node_id = self.handle_pointer_move(event);
                let mut doc = self.doc.inner_mut();
                doc.unactive_node();

                if event.is_primary && matches!(event.id, BlitzPointerId::Finger(_)) {
                    hover_end_event = Some(event.clone());
                }
            }
            UiEvent::PointerLeave(event) => {
                self.handle_pointer_leave(event);
                hover_node_id = None;
            }
            _ => {}
        };

        // A captured pointer's events go to the capturing element.
        let capture = {
            let doc = self.doc.inner();
            doc.pointer_capture()
                .filter(|&id| doc.get_node(id).is_some())
        };
        let target = match event {
            UiEvent::PointerMove(_) if capture.is_some() => capture,
            UiEvent::PointerUp(_) if capture.is_some() => capture,
            UiEvent::PointerCancel(_) if capture.is_some() => capture,
            UiEvent::PointerMove(_) => hover_node_id,
            UiEvent::PointerUp(_) => hover_node_id,
            UiEvent::PointerDown(_) => hover_node_id,
            UiEvent::PointerCancel(_) => hover_node_id,
            UiEvent::PointerLeave(_) => hover_node_id,
            // The element under the wheel. Hover can be stale here: content
            // scrolls under a resting cursor, and a pointer that left the
            // window clears it, but a wheel event carries its own position.
            UiEvent::Wheel(ref w) => {
                let doc = self.doc.inner();
                doc.hit(w.coords.page_x, w.coords.page_y)
                    .and_then(|h| doc.nearest_non_anonymous_ancestor(h.node_id))
                    .or(hover_node_id)
            }
            UiEvent::KeyUp(_) => focussed_node_id,
            UiEvent::KeyDown(_) => focussed_node_id,
            UiEvent::Ime(_) => focussed_node_id,
            UiEvent::AppleStandardKeybinding(_) => focussed_node_id,
        };
        // Fall back to the root element. A document without a root element (e.g. an
        // empty iframe sub-document) has no event target, so there is nothing to do.
        let Some(target) = target.or_else(|| self.doc.inner().try_root_element().map(|el| el.id))
        else {
            return;
        };

        match event {
            UiEvent::PointerMove(data) => {
                self.handle_pointer_event(
                    target,
                    data,
                    DomEventData::PointerMove,
                    Some(DomEventData::MouseMove),
                    DomEventData::TouchMove,
                );
            }
            UiEvent::PointerUp(data) => {
                self.handle_pointer_event(
                    target,
                    data,
                    DomEventData::PointerUp,
                    Some(DomEventData::MouseUp),
                    DomEventData::TouchEnd,
                );
            }
            UiEvent::PointerDown(data) => {
                self.handle_pointer_event(
                    target,
                    data,
                    DomEventData::PointerDown,
                    Some(DomEventData::MouseDown),
                    DomEventData::TouchStart,
                );
            }
            UiEvent::PointerCancel(data) => {
                // `pointercancel` has no mouse-compatibility event, but does
                // generate a `touchcancel` for touch-like inputs.
                self.handle_pointer_event(
                    target,
                    data,
                    DomEventData::PointerCancel,
                    None::<fn(BlitzPointerEvent) -> DomEventData>,
                    DomEventData::TouchCancel,
                );
            }
            // Out/leave events were dispatched by `handle_pointer_leave`
            UiEvent::PointerLeave(_) => {}
            UiEvent::Wheel(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::Wheel(data)))
            }
            UiEvent::KeyUp(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::KeyUp(data)))
            }
            UiEvent::KeyDown(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::KeyDown(data)))
            }
            UiEvent::Ime(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::Ime(data)))
            }
            UiEvent::AppleStandardKeybinding(data) => {
                let mut dom_event =
                    DomEvent::new(target, DomEventData::AppleStandardKeybinding(data));
                self.run_default_action(&mut dom_event);
                self.process_queue();
            }
        };

        // Releasing the button (or a cancel) releases a capture.
        if is_release {
            self.doc.inner_mut().set_pointer_capture(None);
        }

        // Update document input state (hover, focus, active, etc)
        if let Some(event) = hover_end_event {
            self.handle_pointer_leave(&event);
        }

        // Update the cursor once the event (including any forwarding to a
        // sub-document) has been fully handled
        let doc = self.doc.inner();
        if !is_leave && doc.hover_state() != prev_hover {
            doc.update_cursor();
        }
    }

    pub fn handle_dom_event(&mut self, event: DomEvent) {
        self.queue.push_back(event);
        self.process_queue();
    }

    pub(super) fn handle_pointer_event(
        &mut self,
        target: NodeId,
        data: BlitzPointerEvent,
        make_ptr_data: impl FnOnce(BlitzPointerEvent) -> DomEventData,
        make_mouse_data: Option<impl FnOnce(BlitzPointerEvent) -> DomEventData>,
        make_touch_data: impl FnOnce(BlitzPointerEvent) -> DomEventData,
    ) {
        let mut ptr_event = DomEvent::new(target, make_ptr_data(data.clone()));
        let mut event_state = EventState::default();
        event_state = self.run_handler_event(&mut ptr_event, event_state);

        // Generate the corresponding compatibility event (mouse events for the
        // mouse, touch events for fingers and pen/stylus input) and expose it to
        // script. The default action is always run on the pointer event so that
        // the shell layer and default actions remain pointer-based.
        //
        // `pointercancel` has no mouse equivalent, so `make_mouse_data` is `None`
        // in that case and no mouse event is generated.
        if !event_state.is_cancelled() {
            if data.is_mouse() {
                if let Some(make_mouse_data) = make_mouse_data {
                    let mut mouse_event = DomEvent::new(target, make_mouse_data(data));
                    event_state = self.run_handler_event(&mut mouse_event, event_state);
                }
            } else if data.is_finger() || data.is_pen() {
                let mut touch_event = DomEvent::new(target, make_touch_data(data));
                event_state = self.run_handler_event(&mut touch_event, event_state);
            }
        }

        if !event_state.is_cancelled() {
            self.run_default_action(&mut ptr_event);
        }
        self.process_queue();
    }

    pub(super) fn process_queue(&mut self) {
        while let Some(mut event) = self.queue.pop_front() {
            let event_state = self.run_handler_event(&mut event, EventState::default());
            if !event_state.is_cancelled() {
                self.run_default_action(&mut event);
            }
        }
    }

    fn adjust_element_coords(
        &self,
        target: NodeId,
        coords: &PointerCoords,
        element: &mut Point<f32>,
    ) {
        if let Some(rect) = self.doc.inner().get_client_bounding_rect(target) {
            element.x = coords.client_x - rect.x as f32;
            element.y = coords.client_y - rect.y as f32;
        }
    }

    fn run_handler_event(
        &mut self,
        event: &mut DomEvent,
        initial_event_state: EventState,
    ) -> EventState {
        if self.doc.inner().get_node(event.target).is_none() {
            return initial_event_state;
        }
        let chain = if event.bubbles {
            let doc = self.doc.inner();
            doc.node_chain(event.target)
        } else {
            vec![event.target]
        };

        match &mut event.data {
            DomEventData::PointerMove(data)
            | DomEventData::PointerDown(data)
            | DomEventData::PointerUp(data)
            | DomEventData::PointerCancel(data)
            | DomEventData::PointerEnter(data)
            | DomEventData::PointerLeave(data)
            | DomEventData::PointerOver(data)
            | DomEventData::PointerOut(data)
            | DomEventData::MouseMove(data)
            | DomEventData::MouseDown(data)
            | DomEventData::MouseUp(data)
            | DomEventData::MouseEnter(data)
            | DomEventData::MouseLeave(data)
            | DomEventData::MouseOver(data)
            | DomEventData::MouseOut(data)
            | DomEventData::TouchStart(data)
            | DomEventData::TouchEnd(data)
            | DomEventData::TouchMove(data)
            | DomEventData::TouchCancel(data)
            | DomEventData::Click(data)
            | DomEventData::ContextMenu(data)
            | DomEventData::DoubleClick(data) => {
                self.adjust_element_coords(event.target, &data.coords, &mut data.element)
            }
            DomEventData::Wheel(data) => {
                self.adjust_element_coords(event.target, &data.coords, &mut data.element)
            }
            _ => {}
        }

        let mut event_state = initial_event_state;
        self.handler
            .handle_event(&chain, event, self.doc, &mut event_state);

        event_state
    }

    fn run_default_action(&mut self, event: &mut DomEvent) {
        let mut doc = self.doc.inner_mut();
        // The target may be gone: handlers can remove it (script mutates the
        // tree during dispatch, as in browsers). A removed node has no
        // default action.
        if doc.get_node(event.target).is_none() {
            return;
        }
        doc.handle_dom_event(event, |new_evt| self.queue.push_back(new_evt));
    }
}

fn is_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(NamedKey::Alt)
            | Key::Named(NamedKey::AltGraph)
            | Key::Named(NamedKey::CapsLock)
            | Key::Named(NamedKey::Control)
            | Key::Named(NamedKey::Fn)
            | Key::Named(NamedKey::FnLock)
            | Key::Named(NamedKey::Meta)
            | Key::Named(NamedKey::NumLock)
            | Key::Named(NamedKey::ScrollLock)
            | Key::Named(NamedKey::Shift)
            | Key::Named(NamedKey::Symbol)
            | Key::Named(NamedKey::SymbolLock)
            | Key::Named(NamedKey::Hyper)
            | Key::Named(NamedKey::Super)
    )
}
