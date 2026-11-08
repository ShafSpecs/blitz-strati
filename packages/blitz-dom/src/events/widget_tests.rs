use super::handle_dom_event;
use crate::{Attribute, BaseDocument, DocumentConfig, Widget, qual_name};
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta, BlitzWheelEvent, DomEvent,
    DomEventData, MouseEventButton, PointerCoords, UiEvent,
};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::cell::RefCell;
use std::rc::Rc;

struct Record(Rc<RefCell<Vec<UiEvent>>>);

impl Widget for Record {
    fn handle_event(&mut self, event: &UiEvent) {
        self.0.borrow_mut().push(event.clone());
    }
}

#[test]
fn pointer_and_wheel_use_scrolled_content_origin() {
    let mut doc = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(400, 300, 1.25, ColorScheme::Light)),
        ..Default::default()
    });
    let root = doc.root_node().id;
    let mut m = doc.mutate();
    let style = |value: &str| vec![Attribute { name: qual_name!("style"), value: value.into() }];
    let html = m.create_element(qual_name!("html", html), vec![]);
    let body = m.create_element(qual_name!("body", html), style("margin:0;height:1000px"));
    let parent = m.create_element(qual_name!("div", html), style("width:200px;height:100px;overflow:scroll;margin:20px;border:2px solid;padding:5px"));
    let widget = m.create_element(qual_name!("div", html), style("width:101px;height:150px;margin-left:30px;border:3px solid;padding:7px"));
    m.append_children(parent, &[widget]);
    m.append_children(body, &[parent]);
    m.append_children(html, &[body]);
    m.append_children(root, &[html]);
    drop(m);
    let log = Rc::new(RefCell::new(Vec::new()));
    doc.set_custom_widget(widget, Box::new(Record(log.clone())));
    doc.resolve(0.0);
    *doc.nodes[parent].scroll_offset_mut() = crate::util::Point { x: 11.0, y: 13.0 };
    *doc.nodes[widget].scroll_offset_mut() = crate::util::Point { x: 2.0, y: 4.0 };
    doc.set_viewport_scroll(crate::util::Point { x: 5.0, y: 7.0 });

    let layout = doc.nodes[widget].final_layout();
    let mut origin = crate::util::Point {
        x: layout.border.left + layout.padding.left,
        y: layout.border.top + layout.padding.top,
    };
    for id in [widget, parent, body, html] {
        let node = &doc.nodes[id];
        origin.x += node.final_layout().location.x - node.scroll_offset().x as f32;
        origin.y += node.final_layout().location.y - node.scroll_offset().y as f32;
    }
    origin.x -= 5.0;
    origin.y -= 7.0;
    let coords = PointerCoords {
        client_x: origin.x + 9.0, client_y: origin.y + 12.0,
        page_x: origin.x + 14.0, page_y: origin.y + 19.0,
        screen_x: 123.0, screen_y: 456.0,
    };
    let pointer = BlitzPointerEvent {
        id: BlitzPointerId::Mouse, is_primary: true, coords,
        button: MouseEventButton::Main, buttons: Default::default(),
        mods: Default::default(), details: Default::default(),
        element: Default::default(), active_pointers: Default::default(),
    };
    let wheel = BlitzWheelEvent {
        coords, delta: BlitzWheelDelta::Pixels(2.0, -3.0),
        buttons: Default::default(), mods: Default::default(), element: Default::default(),
    };
    for data in [DomEventData::PointerDown(pointer.clone()), DomEventData::PointerMove(pointer), DomEventData::Wheel(wheel)] {
        handle_dom_event(&mut doc, &mut DomEvent::new(widget, data), |_| {});
    }
    let events = log.borrow();
    assert_eq!(events.len(), 3);
    for event in events.iter() {
        let coords = match event {
            UiEvent::PointerDown(p) | UiEvent::PointerMove(p) => p.coords,
            UiEvent::Wheel(w) => {
                assert!(matches!(w.delta, BlitzWheelDelta::Pixels(2.0, -3.0)));
                w.coords
            }
            _ => panic!("unexpected widget event"),
        };
        assert!((coords.client_x - 9.0).abs() < 0.001);
        assert!((coords.client_y - 12.0).abs() < 0.001);
        assert_eq!((coords.screen_x, coords.screen_y), (123.0, 456.0));
    }
}
