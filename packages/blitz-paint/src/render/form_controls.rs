use super::ElementCx;
use crate::color::Color;
use anyrender::PaintScene;
use blitz_dom::local_name;
use kurbo::{Affine, BezPath, Cap, Circle, Join, Point, RoundedRect, Stroke, Vec2};
use peniko::Fill;
use style::dom::TElement as _;

impl ElementCx<'_, '_> {
    pub(super) fn draw_input(&self, scene: &mut impl PaintScene) {
        if self.node.local_name() != "input" {
            return;
        }
        // `appearance: none`: the author styles the box; nothing of the
        // native control is drawn.
        if self.style.get_box().clone_appearance()
            == style::values::specified::box_::Appearance::None
        {
            return;
        }
        if let Some(range) = blitz_dom::RangeState::of(self.element) {
            self.draw_range(scene, &range);
            return;
        }
        let Some(checked) = self.element.checkbox_input_checked() else {
            return;
        };

        let type_attr = self.node.attr(local_name!("type"));
        let disabled = self.node.attr(local_name!("disabled")).is_some();

        // accent-color isn't available from Stylo's servo build: the default
        // accent, as Chromium's (and as the range input uses).
        let accent_color = if disabled {
            Color::from_rgba8(209, 209, 209, 255)
        } else {
            Color::from_rgba8(0, 117, 255, 255)
        };

        // The control fills its border box: the drawing below is laid out
        // for a 20px box.
        let width = self.frame.border_box.width();
        let height = self.frame.border_box.height();
        let min_dimension = width.min(height);
        let scale = min_dimension / 20.0;

        let frame = self.frame.border_box.to_rounded_rect(scale * 2.0);

        match type_attr {
            Some("checkbox") => {
                draw_checkbox(scene, checked, frame, self.transform, accent_color, scale);
            }
            Some("radio") => {
                let center = frame.center();
                draw_radio_button(scene, checked, center, self.transform, accent_color, scale);
            }
            _ => {}
        }
    }
}

impl ElementCx<'_, '_> {
    /// A range input: a 4px track across the content box, filled up to the
    /// thumb (a 16px disc) in the accent colour.
    fn draw_range(&self, scene: &mut impl PaintScene, range: &blitz_dom::RangeState) {
        let disabled = self.node.attr(local_name!("disabled")).is_some();
        // accent-color isn't available from Stylo's servo build: the default
        // accent, as Chromium's.
        let accent = if disabled {
            Color::from_rgba8(160, 160, 160, 255)
        } else {
            Color::from_rgba8(0, 117, 255, 255)
        };
        let track_bg = Color::from_rgba8(128, 128, 128, 110);
        let scale = self.scale;
        let content = self.frame.content_box;
        let thumb = blitz_dom::RANGE_THUMB_SIZE * scale;
        let cy = content.y0 + content.height() / 2.0;
        let left = content.x0 + thumb / 2.0;
        let right = (content.x1 - thumb / 2.0).max(left);
        let x = left + (right - left) * range.fraction();
        let half = 2.0 * scale;
        let track =
            kurbo::Rect::new(content.x0, cy - half, content.x1, cy + half).to_rounded_rect(half);
        scene.fill(
            Fill::NonZero,
            self.transform,
            track_bg,
            None,
            &track,
            anyrender::AntiAliasing::Enabled,
        );
        let filled = kurbo::Rect::new(content.x0, cy - half, x, cy + half).to_rounded_rect(half);
        scene.fill(
            Fill::NonZero,
            self.transform,
            accent,
            None,
            &filled,
            anyrender::AntiAliasing::Enabled,
        );
        scene.fill(
            Fill::NonZero,
            self.transform,
            accent,
            None,
            &Circle::new((x, cy), thumb / 2.0),
            anyrender::AntiAliasing::Enabled,
        );
    }
}

fn draw_checkbox(
    scene: &mut impl PaintScene,
    checked: bool,
    frame: RoundedRect,
    transform: Affine,
    accent_color: Color,
    scale: f64,
) {
    if checked {
        scene.fill(
            Fill::NonZero,
            transform,
            accent_color,
            None,
            &frame,
            anyrender::AntiAliasing::Enabled,
        );
        //Tick code derived from masonry
        let mut path = BezPath::new();
        path.move_to((2.0, 9.0));
        path.line_to((6.0, 13.0));
        path.line_to((14.0, 2.0));

        path.apply_affine(Affine::translate(Vec2 { x: 2.0, y: 1.0 }).then_scale(scale));

        let style = Stroke {
            width: 2.0 * scale,
            join: Join::Round,
            miter_limit: 10.0,
            start_cap: Cap::Round,
            end_cap: Cap::Round,
            dash_pattern: Default::default(),
            dash_offset: 0.0,
        };

        scene.stroke(
            &style,
            transform,
            Color::WHITE,
            None,
            &path,
            anyrender::AntiAliasing::Enabled,
        );
    } else {
        // Unchecked: a grey outline, as Chromium's.
        scene.fill(
            Fill::NonZero,
            transform,
            Color::WHITE,
            None,
            &frame,
            anyrender::AntiAliasing::Enabled,
        );
        scene.stroke(
            &Stroke::default(),
            transform,
            Color::from_rgba8(118, 118, 118, 255),
            None,
            &frame,
            anyrender::AntiAliasing::Enabled,
        );
    }
}

fn draw_radio_button(
    scene: &mut impl PaintScene,
    checked: bool,
    center: Point,
    transform: Affine,
    accent_color: Color,
    scale: f64,
) {
    let outer_ring = Circle::new(center, 8.0 * scale);
    let gap = Circle::new(center, 6.0 * scale);
    let inner_circle = Circle::new(center, 4.0 * scale);
    if checked {
        scene.fill(
            Fill::NonZero,
            transform,
            accent_color,
            None,
            &outer_ring,
            anyrender::AntiAliasing::Enabled,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::WHITE,
            None,
            &gap,
            anyrender::AntiAliasing::Enabled,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            accent_color,
            None,
            &inner_circle,
            anyrender::AntiAliasing::Enabled,
        );
    } else {
        const GRAY: Color = color::palette::css::GRAY;
        scene.fill(
            Fill::NonZero,
            transform,
            GRAY,
            None,
            &outer_ring,
            anyrender::AntiAliasing::Enabled,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::WHITE,
            None,
            &gap,
            anyrender::AntiAliasing::Enabled,
        );
    }
}
