use blitz_traits::events::{BlitzCompositionEvent, BlitzImeEvent, DomEvent, DomEventData};

use crate::BaseDocument;
use blitz_traits::node_id::NodeId;

/// An IME event on the focused text input: applied to its editor, with the
/// DOM events a browser fires. A composition starts with the first preedit
/// (`compositionstart`), updates with each (`compositionupdate`, then
/// `input`), and ends at a commit (`input`, then `compositionend` with the
/// text) or when the IME is disabled with a preedit left (`compositionend`
/// with nothing).
pub(crate) fn handle_ime_event<F: FnMut(DomEvent)>(
    doc: &mut BaseDocument,
    event: BlitzImeEvent,
    mut dispatch_event: F,
) {
    let Some(node_id) = doc.focus_node_id else {
        return;
    };
    let composition = |kind: fn(BlitzCompositionEvent) -> DomEventData, data: &str| {
        DomEvent::new(node_id, kind(BlitzCompositionEvent { data: data.to_owned() }))
    };
    let node = &mut doc.nodes[node_id];
    let Some(input_data) = node.data.downcast_element_mut().and_then(|el| el.text_input_data_mut()) else {
        return;
    };

    if let BlitzImeEvent::Preedit(text, _) = &event {
        if !text.is_empty() && !input_data.composing {
            input_data.composing = true;
            dispatch_event(composition(DomEventData::CompositionStart, ""));
        }
        if input_data.composing {
            dispatch_event(composition(DomEventData::CompositionUpdate, text));
        }
    }
    let ends = match &event {
        BlitzImeEvent::Commit(text) if input_data.composing => Some(text.clone()),
        BlitzImeEvent::Disabled if input_data.composing => Some(String::new()),
        _ => None,
    };
    if ends.is_some() {
        input_data.composing = false;
    }

    let generated_event = input_data.apply_ime_event(
        &mut doc.font_ctx.lock().unwrap(),
        &mut doc.layout_ctx,
        event,
    );
    if let Some(generated_event) = generated_event {
        doc.apply_generated_text_input_event(node_id, generated_event, &mut dispatch_event);
    }
    if let Some(data) = ends {
        dispatch_event(composition(DomEventData::CompositionEnd, &data));
    }
    doc.set_ime_cursor_area_at_caret(node_id);

    #[cfg(feature = "tracing")]
    tracing::debug!(node_id = ?node_id, "Sent ime event");
}

impl BaseDocument {
    /// Tells the platform where the caret of a focused text input is, so the
    /// IME's candidate window opens next to the text being composed.
    pub(crate) fn set_ime_cursor_area_at_caret(&mut self, node_id: NodeId) {
        let node = &self.nodes[node_id];
        let Some(input) = node.element_data().and_then(|el| el.text_input_data()) else {
            return;
        };
        let (Some(caret), Some(layout)) = (input.editor.cursor_geometry(1.0), input.editor.try_layout()) else {
            return;
        };
        // Parley's geometry is at the editor's scale (device pixels).
        let scale = layout.scale() as f64;
        let l = node.final_layout();
        let (dx, dy) = if input.is_multiline { (0.0, input.scroll_offset) } else { (input.scroll_offset, 0.0) };
        let x = (l.border.left + l.padding.left) as f64 + caret.x0 / scale - dx as f64;
        let y = (l.border.top + l.padding.top) as f64 + caret.y0 / scale - dy as f64;
        let height = (caret.y1 - caret.y0) / scale;
        // Through transforms on the way up, as a dialog centered with
        // `translate(-50%, -50%)` is.
        let mut pts = [kurbo::Point::new(x, y), kurbo::Point::new(x, y + height)];
        self.map_to_document(node_id, &mut pts);
        let scroll = self.viewport_scroll();
        let height = (pts[1].y - pts[0].y).abs();
        self.shell_provider.set_ime_cursor_area((pts[0].x - scroll.x) as f32, (pts[0].y - scroll.y) as f32, 1.0, height as f32);
    }

    /// Places the IME at a text input's content box, before it has a caret.
    pub(crate) fn set_ime_cursor_area_at_field(&mut self, node_id: NodeId) {
        let l = *self.nodes[node_id].final_layout();
        let (x, y) = ((l.border.left + l.padding.left) as f64, (l.border.top + l.padding.top) as f64);
        let mut pts = [kurbo::Point::new(x, y), kurbo::Point::new(x + l.content_box_width() as f64, y + l.content_box_height() as f64)];
        self.map_to_document(node_id, &mut pts);
        let (w, h) = ((pts[1].x - pts[0].x).abs(), (pts[1].y - pts[0].y).abs());
        self.shell_provider.set_ime_cursor_area(pts[0].x as f32, pts[0].y as f32, w as f32, h as f32);
    }
}

#[cfg(test)]
mod tests {
    use crate::{Attribute, BaseDocument, Document, DocumentConfig, EventDriver, EventHandler, QualName, qual_name};
    use blitz_traits::events::{BlitzImeEvent as E, DomEvent, EventState, UiEvent};
    use blitz_traits::node_id::NodeId;
    use blitz_traits::shell::{ColorScheme, Viewport};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Record(Rc<RefCell<Vec<String>>>);
    impl EventHandler for Record {
        fn handle_event(&mut self, _: &[NodeId], event: &mut DomEvent, _: &mut dyn Document, _: &mut EventState) {
            let detail = match &event.data {
                blitz_traits::events::DomEventData::CompositionStart(c)
                | blitz_traits::events::DomEventData::CompositionUpdate(c)
                | blitz_traits::events::DomEventData::CompositionEnd(c) => format!("({})", c.data),
                blitz_traits::events::DomEventData::Input(i) => format!("({})", i.value),
                _ => String::new(),
            };
            self.0.borrow_mut().push(format!("{}{detail}", event.data.name()));
        }
    }

    /// The DOM events of a composition that commits, then of one cancelled,
    /// in the order browsers fire them; the value carries the preedit.
    #[test]
    fn composition_events_in_order() {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html", html), vec![]);
        let body = m.create_element(qual_name!("body", html), vec![]);
        let attr = |name: QualName, value: &str| Attribute { name, value: value.to_string() };
        let input = m.create_element(qual_name!("input", html), vec![attr(qual_name!("type"), "text")]);
        m.append_children(body, &[input]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);
        doc.set_focus_to(input);

        let log = Rc::new(RefCell::new(Vec::new()));
        let sequence = [
            E::Enabled,
            E::Preedit("k".into(), Some((1, 1))),
            E::Preedit("ka".into(), Some((2, 2))),
            E::Preedit(String::new(), None),
            E::Commit("か".into()),
            E::Preedit("x".into(), Some((1, 1))),
            E::Preedit(String::new(), None),
            E::Disabled,
        ];
        for event in sequence {
            EventDriver::new(&mut doc, Record(log.clone())).handle_ui_event(UiEvent::Ime(event));
        }
        // The raw IME event (named "composition") isn't a DOM event; skip it.
        let got: Vec<String> = log.borrow().iter().filter(|e| *e != "composition").cloned().collect();
        assert_eq!(
            got,
            [
                "compositionstart()",
                "compositionupdate(k)",
                "input(k)",
                "compositionupdate(ka)",
                "input(ka)",
                "compositionupdate()",
                "input()",
                "input(か)",
                "compositionend(か)",
                "compositionstart()",
                "compositionupdate(x)",
                "input(かx)",
                "compositionupdate()",
                "input(か)",
                "compositionend()",
            ]
        );
    }

    type Log = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
    struct ShellLog(Log, std::sync::Arc<std::sync::Mutex<Vec<(f32, f32)>>>);
    impl ShellLog {
        fn new(calls: &Log) -> Self {
            ShellLog(calls.clone(), Default::default())
        }
    }
    impl blitz_traits::shell::ShellProvider for ShellLog {
        fn set_ime_enabled(&self, is_enabled: bool) {
            self.0.lock().unwrap().push(format!("enabled({is_enabled})"));
        }
        fn set_ime_cursor_area(&self, x: f32, y: f32, _: f32, _: f32) {
            self.0.lock().unwrap().push("area".into());
            self.1.lock().unwrap().push((x, y));
        }
    }

    /// Focusing a text input enables the IME and places it; a password
    /// field gets no IME, as in browsers, also when a focused
    /// field's type changes.
    #[test]
    fn focus_enables_the_ime_except_for_passwords() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            shell_provider: Some(std::sync::Arc::new(ShellLog::new(&calls))),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html", html), vec![]);
        let body = m.create_element(qual_name!("body", html), vec![]);
        let attr = |name: QualName, value: &str| Attribute { name, value: value.to_string() };
        let text = m.create_element(qual_name!("input", html), vec![attr(qual_name!("type"), "text")]);
        let password = m.create_element(qual_name!("input", html), vec![attr(qual_name!("type"), "password")]);
        m.append_children(body, &[text, password]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);

        doc.set_focus_to(text);
        assert_eq!(calls.lock().unwrap()[..], ["enabled(true)", "area"]);
        calls.lock().unwrap().clear();
        doc.set_focus_to(password);
        assert!(!calls.lock().unwrap().contains(&"enabled(true)".to_string()), "{:?}", calls.lock().unwrap());

        // "Show password": the focused field becomes a text field, and back.
        calls.lock().unwrap().clear();
        doc.mutate().set_attribute(password, qual_name!("type"), "text");
        assert_eq!(calls.lock().unwrap()[..], ["enabled(true)", "area"]);
        calls.lock().unwrap().clear();
        doc.mutate().set_attribute(password, qual_name!("type"), "password");
        assert_eq!(calls.lock().unwrap()[..], ["enabled(false)"]);
    }

    /// The IME is placed at the field's content box and caret, in document
    /// coordinates: x = body margin 8 + input margin 50 + border 2 + padding 3;
    /// y = the input's 20px top margin (collapsed with the body's 8) + 2 + 3.
    #[test]
    fn the_ime_is_placed_at_the_caret() {
        let calls = Log::default();
        let shell = std::sync::Arc::new(ShellLog::new(&calls));
        let areas = shell.1.clone();
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            shell_provider: Some(shell),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html", html), vec![]);
        let body = m.create_element(qual_name!("body", html), vec![]);
        let style = "display:block;margin:20px 0 0 50px;border:2px solid;padding:3px";
        let input = m.create_element(
            qual_name!("input", html),
            vec![Attribute { name: qual_name!("style"), value: style.to_string() }],
        );
        m.append_children(body, &[input]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);

        doc.set_focus_to(input);
        let areas = areas.lock().unwrap();
        assert!(!areas.is_empty(), "{:?}", calls.lock().unwrap());
        for &(x, y) in areas.iter() {
            assert!((x - 63.0).abs() < 1.0 && (y - 25.0).abs() < 1.0, "{areas:?}");
        }
    }

    /// In a dialog centered with `translate(-50%, -50%)`, the IME follows the
    /// transform: the 200x100 box at (200, 150) is drawn at (100, 100), so its
    /// input's content box is at x = 100 + 2 + 3, y = 100 + 2 + 3.
    #[test]
    fn the_ime_follows_a_transformed_dialog() {
        let calls = Log::default();
        let shell = std::sync::Arc::new(ShellLog::new(&calls));
        let areas = shell.1.clone();
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            shell_provider: Some(shell),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html", html), vec![]);
        let body = m.create_element(qual_name!("body", html), vec![]);
        let style = |value: &str| vec![Attribute { name: qual_name!("style"), value: value.to_string() }];
        let dialog = m.create_element(
            qual_name!("div", html),
            style("position:fixed;left:200px;top:150px;width:200px;height:100px;transform:translate(-50%,-50%)"),
        );
        let input = m.create_element(qual_name!("input", html), style("display:block;margin:0;border:2px solid;padding:3px"));
        m.append_children(dialog, &[input]);
        m.append_children(body, &[dialog]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);

        doc.set_focus_to(input);
        EventDriver::new(&mut doc, crate::NoopEventHandler).handle_ui_event(UiEvent::Ime(E::Preedit("k".into(), Some((1, 1)))));
        let areas = areas.lock().unwrap();
        assert!(areas.len() >= 2, "{:?}", calls.lock().unwrap());
        // At the field, then at the caret after "k" (untransformed: (205, 155)).
        assert!((areas[0].0 - 105.0).abs() < 1.0 && (areas[0].1 - 105.0).abs() < 1.0, "{areas:?}");
        let (x, y) = *areas.last().unwrap();
        assert!(x > 106.0 && x < 125.0 && (y - 105.0).abs() < 1.0, "{areas:?}");
    }
}
