//! Drop-down selects: the closed control (the selected option's label and an
//! arrow), and the picker an open select draws over the page.

use super::{BlitzDomPainter, ElementCx};
use crate::color::{Color, ToColorColor as _};
use anyrender::PaintScene;
use blitz_dom::node::SpecialElementData;
use kurbo::{Affine, BezPath, Cap, Join, Rect, Stroke};
use peniko::Fill;

/// The picker's colours: a light popup, as browsers draw it on Windows; the
/// highlighted option in the default accent (as the other form controls).
const PICKER_BACKGROUND: Color = Color::from_rgba8(255, 255, 255, 255);
const PICKER_BORDER: Color = Color::from_rgba8(118, 118, 118, 255);
const PICKER_TEXT: Color = Color::from_rgba8(0, 0, 0, 255);
const PICKER_DISABLED_TEXT: Color = Color::from_rgba8(109, 109, 109, 255);
const PICKER_HIGHLIGHT: Color = Color::from_rgba8(0, 117, 255, 255);
const PICKER_HIGHLIGHT_TEXT: Color = Color::from_rgba8(255, 255, 255, 255);

impl ElementCx<'_, '_> {
    /// A drop-down select: the selected option's label in the content box,
    /// and the arrow at its end (not with `appearance: none`).
    pub(super) fn draw_select(&self, scene: &mut impl PaintScene) {
        let SpecialElementData::Select(data) = &self.element.special_data else {
            return;
        };
        let content = self.frame.content_box;
        let font_size = self.style.clone_font_size().computed_size().px();
        let arrow = blitz_dom::select_arrow_width(font_size) as f64 * self.scale;
        let color = self.style.clone_color().as_srgb_color();

        if let Some(label) = &data.label {
            let y = content.y0 + ((content.height() - label.height() as f64) / 2.0).max(0.0);
            let room = Rect::new(content.x0, content.y0, (content.x1 - arrow).max(content.x0), content.y1);
            scene.push_clip_layer(Fill::NonZero, self.transform, &room);
            crate::text::fill_text_in_color(
                scene,
                label.lines(),
                self.transform * Affine::translate((content.x0, y)),
                self.scale,
                color,
            );
            scene.pop_layer();
        }

        let appearance_none =
            self.style.get_box().clone_appearance() == style::values::specified::box_::Appearance::None;
        if !appearance_none {
            let (cx, cy) = (content.x1 - arrow / 2.0, content.y0 + content.height() / 2.0);
            let w = (font_size as f64 * 0.55 * self.scale).round();
            let h = w * 0.55;
            let mut chevron = BezPath::new();
            chevron.move_to((cx - w / 2.0, cy - h / 2.0));
            chevron.line_to((cx, cy + h / 2.0));
            chevron.line_to((cx + w / 2.0, cy - h / 2.0));
            let stroke = Stroke::new(1.5 * self.scale)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            scene.stroke(
                &stroke,
                self.transform,
                color,
                None,
                &chevron,
                anyrender::AntiAliasing::Enabled,
            );
        }
    }
}

impl BlitzDomPainter<'_, '_> {
    /// The open select's picker, above everything else.
    pub(super) fn draw_select_picker(&self, scene: &mut impl PaintScene) {
        let Some((_, picker)) = self.dom.as_ref().open_picker() else {
            return;
        };
        let scale = self.scale;
        // Picker geometry is in CSS px, viewport coordinates.
        let to_device = Affine::translate((self.initial_x, self.initial_y)) * Affine::scale(scale);

        scene.fill(
            Fill::NonZero,
            to_device,
            PICKER_BACKGROUND,
            None,
            &picker.rect,
            anyrender::AntiAliasing::Enabled,
        );
        scene.push_clip_layer(Fill::NonZero, to_device, &picker.rect.inset(-1.0));
        for (i, row) in picker.rows.iter().enumerate() {
            let r = picker.row_rect(i);
            if r.y1 < picker.rect.y0 || r.y0 > picker.rect.y1 {
                continue;
            }
            let highlighted = picker.highlighted == Some(i);
            if highlighted {
                scene.fill(
                    Fill::NonZero,
                    to_device,
                    PICKER_HIGHLIGHT,
                    None,
                    &r,
                    anyrender::AntiAliasing::Enabled,
                );
            }
            let color = if highlighted {
                PICKER_HIGHLIGHT_TEXT
            } else if row.disabled {
                PICKER_DISABLED_TEXT
            } else {
                PICKER_TEXT
            };
            let indent = if row.indented { blitz_dom::PICKER_INDENT } else { 0.0 };
            let label_height = row.label.height() as f64 / scale;
            let x = r.x0 + blitz_dom::PICKER_PADDING + indent;
            let y = r.y0 + ((r.height() - label_height) / 2.0).max(0.0);
            crate::text::fill_text_in_color(
                scene,
                row.label.lines(),
                Affine::translate((self.initial_x + x * scale, self.initial_y + y * scale)),
                scale,
                color,
            );
        }
        scene.pop_layer();
        let border = picker.rect.inset(-0.5);
        scene.stroke(
            &Stroke::new(1.0),
            to_device,
            PICKER_BORDER,
            None,
            &border,
            anyrender::AntiAliasing::Enabled,
        );
    }
}
