use super::{WidgetGeometry, paint_scene};
use anyrender::{Paint, PaintScene, Scene, recording::RenderCommand};
use blitz_dom::{Attribute, BaseDocument, DocumentConfig, Widget, node::ComputedStyles, qual_name};
use blitz_traits::shell::{ColorScheme, Viewport};
use kurbo::{Affine, Rect, Shape};
use peniko::{Color, Fill};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn sizes_snap_both_device_edges() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        for (x, width) in [(37.3, 101.0), (0.4, 0.4), (0.0, 100.0), (-0.4, 0.4)] {
            let origin = x * scale;
            let extent = width * scale;
            let geometry = WidgetGeometry::new(Affine::translate((origin, origin)), Rect::new(0.0, 0.0, extent, extent));
            let expected = ((origin + extent).round() - origin.round()) as u32;
            assert_eq!((geometry.width, geometry.height), (expected, expected));
            assert_eq!(geometry.transform * kurbo::Point::ZERO, kurbo::Point::new(origin, origin));
        }
    }
    assert_eq!(WidgetGeometry::new(Affine::IDENTITY, Rect::new(0.4, 0.4, 0.8, 0.8)).width, 1);
    assert_eq!(WidgetGeometry::new(Affine::IDENTITY, Rect::new(0.1, 0.1, 0.4, 0.4)).width, 0);
}

struct Record(Rc<RefCell<Vec<(u32, u32, f64)>>>);

impl Widget for Record {
    fn paint(&mut self, _: &mut dyn anyrender::RenderContext, _: &ComputedStyles, width: u32, height: u32, scale: f64) -> Scene {
        self.0.borrow_mut().push((width, height, scale));
        let mut scene = Scene::new();
        scene.fill(Fill::NonZero, Affine::IDENTITY, Color::from_rgb8(1, 2, 3), None, &Rect::new(0.0, 0.0, width as f64, height as f64));
        scene
    }
}

#[test]
fn paint_uses_the_measured_content_geometry_after_scroll_and_translation() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, scale as f32, ColorScheme::Light)),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let style = |value: &str| vec![Attribute { name: qual_name!("style"), value: value.into() }];
        let html = m.create_element(qual_name!("html", html), vec![]);
        let body = m.create_element(qual_name!("body", html), style("margin:0;height:1000px"));
        let parent = m.create_element(qual_name!("div", html), style("width:300px;height:200px;overflow:scroll"));
        let widget = m.create_element(qual_name!("div", html), style("width:101px;height:51px;border:3px solid;padding:7px;transform:translate(2.3px, 3.7px)"));
        m.append_children(parent, &[widget]);
        m.append_children(body, &[parent]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        let log = Rc::new(RefCell::new(Vec::new()));
        doc.set_custom_widget(widget, Box::new(Record(log.clone())));
        doc.resolve(0.0);
        let widget_node = doc.get_node_mut(widget).unwrap();
        widget_node.final_layout_mut().location.x = 37.3;
        widget_node.final_layout_mut().location.y = 40.4;
        widget_node.set_transform(scale as f32);
        *doc.get_node_mut(parent).unwrap().scroll_offset_mut() = blitz_dom::util::Point { x: 11.2, y: 13.4 };
        doc.set_viewport_scroll(blitz_dom::util::Point { x: 5.1, y: 7.2 });

        let layout = doc.get_node(widget).unwrap().final_layout();
        let pb_x = (layout.border.left + layout.padding.left) as f64;
        let pb_y = (layout.border.top + layout.padding.top) as f64;
        let css_width = layout.size.width as f64 - pb_x - (layout.border.right + layout.padding.right) as f64;
        let css_height = layout.size.height as f64 - pb_y - (layout.border.bottom + layout.padding.bottom) as f64;
        let x = (layout.location.x as f64 + pb_x + 2.3 - 11.2 - 5.1) * scale;
        let y = (layout.location.y as f64 + pb_y + 3.7 - 13.4 - 7.2) * scale;
        let width = ((x + css_width * scale).round() - x.round()) as u32;
        let height = ((y + css_height * scale).round() - y.round()) as u32;
        let mut scene = Scene::new();
        paint_scene(&mut scene, &mut doc, scale, 800, 600, 0, 0);
        assert_eq!(*log.borrow(), [(width, height, scale)]);
        let fill = scene.commands.iter().find_map(|cmd| match cmd {
            RenderCommand::Fill(fill) if fill.brush == Paint::Solid(Color::from_rgb8(1, 2, 3)) => Some(fill),
            _ => None,
        }).expect("widget marker");
        let rect = fill.shape.bounding_box();
        let origin = fill.transform * rect.origin();
        assert!((origin.x - x).abs() < 0.001, "{origin:?} != ({x}, {y})");
        assert!((origin.y - y).abs() < 0.001);
        assert_eq!((rect.width(), rect.height()), (width as f64, height as f64));
        assert_eq!(origin.x.round() + rect.width(), (x + css_width * scale).round());
        assert_eq!(origin.y.round() + rect.height(), (y + css_height * scale).round());
    }
}

#[test]
fn zero_area_widgets_are_not_painted() {
    let mut doc = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(400, 300, 1.25, ColorScheme::Light)),
        ..Default::default()
    });
    let root = doc.root_node().id;
    let mut m = doc.mutate();
    let html = m.create_element(qual_name!("html", html), vec![]);
    let widget = m.create_element(qual_name!("div", html), vec![Attribute {
        name: qual_name!("style"), value: "width:0;height:0".into(),
    }]);
    m.append_children(html, &[widget]);
    m.append_children(root, &[html]);
    drop(m);
    let log = Rc::new(RefCell::new(Vec::new()));
    doc.set_custom_widget(widget, Box::new(Record(log.clone())));
    doc.resolve(0.0);
    paint_scene(&mut Scene::new(), &mut doc, 1.25, 400, 300, 0, 0);
    assert!(log.borrow().is_empty());
}
