//! `<input type="range">`: the value sanitization, stepping and pointer
//! mapping of the HTML spec's range state. The current value lives in the
//! element's `value` attribute (the control has no separate dirty value), so
//! a value set by script and one set by the user are the same thing.
//!
//! <https://html.spec.whatwg.org/multipage/input.html#range-state-(type=range)>

use blitz_traits::events::{BlitzInputEvent, DomEvent, DomEventData};
use blitz_traits::node_id::NodeId;
use markup5ever::local_name;

use crate::{BaseDocument, ElementData, qual_name};

/// Diameter of the thumb, in CSS px (the track is inset by half of it).
pub const THUMB_SIZE: f64 = 16.0;

/// A range input's bounds, step and (sanitized) value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeState {
    pub min: f64,
    pub max: f64,
    /// `None` for `step="any"`.
    pub step: Option<f64>,
    pub value: f64,
    /// Decimal places of `min` and `step`: values are formatted with at most
    /// this many, so 0.05 steps read "1.55", not "1.5500000000000003".
    decimals: usize,
}

fn decimals(s: Option<&str>) -> usize {
    s.and_then(|s| s.trim().split_once('.'))
        .map_or(0, |(_, f)| f.trim_end_matches('0').len())
}

impl RangeState {
    pub fn of(el: &ElementData) -> Option<Self> {
        if el.name.local != local_name!("input")
            || !el
                .attr(local_name!("type"))
                .is_some_and(|t| t.eq_ignore_ascii_case("range"))
        {
            return None;
        }
        let num = |name| {
            el.attr(name)
                .and_then(|v: &str| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite())
        };
        let min = num(local_name!("min")).unwrap_or(0.0);
        // A maximum below the minimum is the minimum.
        let max = num(local_name!("max")).unwrap_or(100.0).max(min);
        let step_attr = el.attr(local_name!("step"));
        let step = match step_attr {
            Some(s) if s.trim().eq_ignore_ascii_case("any") => None,
            _ => Some(num(local_name!("step")).filter(|s| *s > 0.0).unwrap_or(1.0)),
        };
        let decimals = decimals(el.attr(local_name!("min")))
            .max(decimals(step_attr))
            .min(12);
        let mut state = RangeState {
            min,
            max,
            step,
            value: min,
            decimals,
        };
        // The default value is the midpoint.
        let raw = num(local_name!("value")).unwrap_or(min + (max - min) / 2.0);
        state.value = state.sanitize(raw);
        Some(state)
    }

    /// Clamped to [min, max] and snapped to the step (from `min`); a snap past
    /// the maximum steps back down.
    pub fn sanitize(&self, v: f64) -> f64 {
        let mut v = v.clamp(self.min, self.max);
        if let Some(step) = self.step {
            let n = ((v - self.min) / step).round();
            v = self.min + n * step;
            if v > self.max + 1e-9 {
                v -= step;
            }
            v = v.clamp(self.min, self.max);
        }
        let scale = 10f64.powi(self.decimals as i32);
        (v * scale).round() / scale
    }

    /// The value as the input event and the attribute carry it.
    pub fn format(&self, v: f64) -> String {
        let s = format!("{:.*}", self.decimals, v);
        let s = if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s
        };
        if s == "-0" { "0".to_owned() } else { s }
    }

    /// Where the value sits along the track, 0..=1.
    pub fn fraction(&self) -> f64 {
        if self.max > self.min {
            (self.value - self.min) / (self.max - self.min)
        } else {
            0.0
        }
    }

    /// The value at a fraction 0..=1 of the track.
    pub fn at_fraction(&self, t: f64) -> f64 {
        self.sanitize(self.min + t.clamp(0.0, 1.0) * (self.max - self.min))
    }

    /// One step (arrow keys): the step, or 1% of the range for `step="any"`.
    pub fn small_step(&self) -> f64 {
        self.step.unwrap_or((self.max - self.min) / 100.0)
    }

    /// A large step (Page Up/Down): a tenth of the range, at least one step.
    pub fn large_step(&self) -> f64 {
        ((self.max - self.min) / 10.0).max(self.small_step())
    }
}

impl BaseDocument {
    /// Sets a range input's value (sanitized) and fires `input` when it
    /// changed. Returns whether it changed.
    pub(crate) fn set_range_value(
        &mut self,
        node_id: NodeId,
        value: f64,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        let Some(state) = self.nodes[node_id].element_data().and_then(RangeState::of) else {
            return false;
        };
        let new = state.sanitize(value);
        let current = self.nodes[node_id]
            .element_data()
            .and_then(|el| el.attr(local_name!("value")).map(str::to_owned));
        let text = state.format(new);
        if new == state.value && current.as_deref() == Some(text.as_str()) {
            return false;
        }
        // Attributes are in no namespace (as the parser creates them).
        self.mutate()
            .set_attribute(node_id, qual_name!("value"), &text);
        self.shell_provider.request_redraw();
        dispatch_event(DomEvent::new(
            node_id,
            DomEventData::Input(BlitzInputEvent { value: text }),
        ));
        true
    }

    /// The range value under a pointer at page x (CSS px).
    pub(crate) fn range_value_at(&self, node_id: NodeId, page_x: f32) -> Option<f64> {
        let node = &self.nodes[node_id];
        let state = node.element_data().and_then(RangeState::of)?;
        let layout = node.final_layout();
        let left = node.absolute_position(0.0, 0.0).x as f64
            + (layout.border.left + layout.padding.left) as f64;
        let width = layout.content_box_width() as f64;
        let travel = (width - THUMB_SIZE).max(1.0);
        Some(state.at_fraction((page_x as f64 - left - THUMB_SIZE / 2.0) / travel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(min: f64, max: f64, step: Option<f64>, decimals: usize) -> RangeState {
        RangeState {
            min,
            max,
            step,
            value: min,
            decimals,
        }
    }

    #[test]
    fn snaps_to_steps_from_min_and_formats_without_float_noise() {
        let s = state(0.0, 2.0, Some(0.05), 2);
        assert_eq!(s.format(s.sanitize(1.5 + 0.05)), "1.55");
        assert_eq!(s.format(s.sanitize(1.52)), "1.5");
        assert_eq!(s.format(s.sanitize(2.0)), "2");
        assert_eq!(s.format(s.sanitize(-1.0)), "0");
        assert_eq!(s.format(s.sanitize(9.0)), "2");
    }

    #[test]
    fn a_snap_past_max_steps_back() {
        // min 0, max 10, step 3: 10 snaps to 9 (12 would be past the max).
        let s = state(0.0, 10.0, Some(3.0), 0);
        assert_eq!(s.sanitize(10.0), 9.0);
        assert_eq!(s.sanitize(8.0), 9.0);
    }

    #[test]
    fn fractions_map_across_the_track() {
        let s = state(0.0, 2.0, Some(0.05), 2);
        assert_eq!(s.at_fraction(0.75), 1.5);
        assert_eq!(s.at_fraction(-1.0), 0.0);
        assert_eq!(s.at_fraction(2.0), 2.0);
        assert_eq!(s.large_step(), 0.2);
        assert_eq!(state(0.0, 100.0, None, 0).small_step(), 1.0);
    }
}
