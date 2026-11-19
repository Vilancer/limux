//! The "working" spinner of the sidebar and the tab bar.
//!
//! Drawn and turned by Limux itself, on a timer, while it is on screen.
//! `gtk::Spinner` turns through a CSS animation that can stop for good after
//! the widget is hidden and shown again, which left a still spinner next to
//! a terminal that was working.

use std::f64::consts::TAU;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

/// One turn.
const TURN: Duration = Duration::from_millis(900);
/// Time between redraws while it is on screen.
const STEP: Duration = Duration::from_millis(60);
pub(crate) const CSS_CLASS: &str = "limux-working-spinner";

/// A small spinner in the text colour. It turns whenever it is on screen and
/// stands still when animations are off.
pub(crate) fn new() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(10);
    area.set_content_height(10);
    area.set_valign(gtk::Align::Center);
    area.set_halign(gtk::Align::Center);
    area.set_can_target(false);
    area.add_css_class(CSS_CLASS);
    area.set_draw_func(|area, cr, width, height| {
        let color = area.color();
        let size = f64::from(width.min(height));
        let line = (size / 6.0).max(1.2);
        let radius = (size - line) / 2.0;
        let turn = if animations_on(area) {
            turn_at(glib::monotonic_time())
        } else {
            0.0
        };
        cr.translate(f64::from(width) / 2.0, f64::from(height) / 2.0);
        cr.rotate(turn * TAU);
        cr.set_line_width(line);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        // A faint ring and a bright three-quarter arc on it.
        cr.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            f64::from(color.alpha()) * 0.25,
        );
        cr.arc(0.0, 0.0, radius, 0.0, TAU);
        let _ = cr.stroke();
        cr.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            f64::from(color.alpha()),
        );
        cr.arc(0.0, 0.0, radius, 0.0, TAU * 0.3);
        let _ = cr.stroke();
    });
    // Redraw on a timer only while mapped; the timer ends itself once the
    // widget is unmapped or gone.
    area.connect_map(|area| {
        let weak = area.downgrade();
        glib::timeout_add_local(STEP, move || match weak.upgrade() {
            Some(area) if area.is_mapped() => {
                area.queue_draw();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    });
    area
}

fn animations_on(widget: &gtk::DrawingArea) -> bool {
    widget.settings().is_gtk_enable_animations()
}

/// How far round the spinner is at `micros` of monotonic time, in `0..1`.
fn turn_at(micros: i64) -> f64 {
    let turn = i64::try_from(TURN.as_micros()).unwrap_or(i64::MAX);
    micros.rem_euclid(turn) as f64 / turn as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spinner_turns_with_time() {
        assert_eq!(turn_at(0), 0.0);
        assert_eq!(turn_at(450_000), 0.5);
        assert_eq!(turn_at(900_000), 0.0, "a full turn");
        assert!(turn_at(1_000_000) > 0.0);
    }
}
