#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_term::render::{Shared, Surface};
    use crate::native_term::screen::Frame;
    use crate::native_term::style::{Style, Theme, font_for};
    use alacritty_terminal::vte::ansi::Rgb;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    /// How long the display row waits for one step: the window to map or a frame callback.
    const STEP_WAIT: Duration = Duration::from_secs(10);

    /// How long the display row waits for the farm to take a step's screenshot.
    const SHOT_WAIT: Duration = Duration::from_secs(60);

    /// The longest one present may take: under the 16.7 ms of a 60 Hz output, so a present
    /// that waited on a frame callback fails the row.
    const PRESENT_MS_MAX: f64 = 12.0;

    /// How many presents the display row paces back to back.
    const FRAMES: usize = 60;

    /// The child's colour, which the farm's pixel checks look for.
    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };

    #[test]
    fn a_host_rect_is_finite_bounded_and_at_least_one_pixel() {
        assert_eq!(
            rect((10.4, 20.6, 400.0, 240.0), (0, 30)),
            Ok(Rect {
                x: 10,
                y: 51,
                w: 400,
                h: 240
            })
        );
        assert_eq!(
            rect((-3.0, 0.0, 1.0, 1.0), (0, 0)),
            Ok(Rect {
                x: -3,
                y: 0,
                w: 1,
                h: 1
            })
        );
        let max = f64::from(RECT_MAX);
        for bad in [
            (f64::NAN, 0.0, 10.0, 10.0),
            (0.0, f64::INFINITY, 10.0, 10.0),
            (0.0, 0.0, 0.5, 10.0),
            (0.0, 0.0, 10.0, 0.0),
            (max + 1.0, 0.0, 10.0, 10.0),
            (0.0, 0.0, max + 1.0, 10.0),
        ] {
            assert_eq!(
                rect(bad, (0, 0)),
                Err("native_terminal_rect_invalid"),
                "{bad:?}"
            );
        }
        assert_eq!(
            rect((0.0, 0.0, 10.0, 10.0), (RECT_MAX + 1, 0)),
            Err("native_terminal_rect_invalid")
        );
    }

    #[test]
    fn device_pixels_scale_the_logical_rect() {
        let logical = Rect {
            x: -5,
            y: 7,
            w: 400,
            h: 240,
        };
        assert_eq!(logical.device(1), (-5, 7, 400, 240));
        assert_eq!(logical.device(2), (-10, 14, 800, 480));
    }

    #[test]
    #[should_panic(expected = "a display scale")]
    fn a_scale_out_of_range_is_a_bug() {
        let one = Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        let _ = one.device(0);
    }

    #[test]
    fn a_child_presents_only_with_a_change_and_no_frame_callback_owed() {
        let mut pacing = Pacing::new();
        assert!(pacing.ready());
        pacing.presenting();
        assert!(!pacing.ready());
        pacing.changed();
        assert!(!pacing.ready());
        pacing.frame_done();
        assert!(pacing.ready());
        pacing.presenting();
        pacing.frame_done();
        assert!(!pacing.ready());
        pacing.changed();
        assert!(pacing.ready());
    }

    #[test]
    fn a_hidden_child_never_presents_and_showing_redraws_it() {
        let mut pacing = Pacing::new();
        pacing.presenting();
        pacing.hide();
        pacing.changed();
        pacing.frame_done();
        assert!(!pacing.ready());
        pacing.show();
        assert!(pacing.ready());
        pacing.presenting();
        pacing.frame_done();
        pacing.hide();
        pacing.show();
        assert!(pacing.ready());
    }

    #[test]
    fn a_present_that_did_not_happen_is_owed_again() {
        let mut pacing = Pacing::new();
        pacing.presenting();
        pacing.not_presented();
        assert!(pacing.ready());
    }

    #[test]
    fn a_child_whose_toplevel_unmapped_never_presents_again() {
        let mut pacing = Pacing::new();
        pacing.gone();
        pacing.show();
        pacing.changed();
        pacing.frame_done();
        assert!(!pacing.ready());
        assert!(pacing.is_gone());
    }

    #[test]
    #[should_panic(expected = "a present only when ready")]
    fn presenting_when_not_ready_is_a_bug() {
        let mut pacing = Pacing::new();
        pacing.hide();
        pacing.presenting();
    }

    fn pump() {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
    }

    fn pump_for(wait: Duration) {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            pump();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn ms(since: Instant) -> f64 {
        since.elapsed().as_secs_f64() * 1000.0
    }

    fn stats(samples: &[f64]) -> Value {
        assert!(!samples.is_empty());
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        json!({"n": sorted.len(), "p50": at(0.5), "p95": at(0.95), "max": at(1.0)})
    }

    fn row_dir() -> Option<PathBuf> {
        std::env::var_os("NATIVE_ROW_DIR").map(PathBuf::from)
    }

    fn wait_ready(child: &mut Child) -> f64 {
        let started = Instant::now();
        loop {
            pump();
            if child.ready() {
                return ms(started);
            }
            assert!(
                started.elapsed() < STEP_WAIT,
                "no frame callback within the step wait"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn checkpoint(step: &str, child: &Child) {
        let Some(dir) = row_dir() else {
            return;
        };
        let (x, y, w, h) = child.device_rect();
        let line = json!({
            "step": step,
            "device": [x, y, w, h],
            "root": child.root_origin().map(|(rx, ry)| [rx, ry]),
            "scale": child.scale(),
        });
        std::fs::write(dir.join(format!("{step}.json")), format!("{line}\n")).expect("step file");
        let go = dir.join(format!("go-{step}"));
        let started = Instant::now();
        while !go.exists() {
            assert!(
                started.elapsed() < SHOT_WAIT,
                "the farm took no {step} shot"
            );
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn present(
        child: &mut Child,
        shared: &mut Shared,
        surface: &mut Surface,
        frame: &Frame,
    ) -> f64 {
        child.presenting();
        let started = Instant::now();
        shared.draw(surface, frame).expect("a present");
        ms(started)
    }

    fn white_toplevel() -> gtk::Window {
        gtk::init().expect("a display");
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_decorated(false);
        window.set_default_size(640, 480);
        let css = gtk::CssProvider::new();
        css.load_from_data(b"window { background-color: #ffffff; }")
            .expect("page css");
        let screen = WidgetExt::screen(&window).expect("a screen");
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        window.show_all();
        let started = Instant::now();
        while !window.window().is_some_and(|gdk| gdk.is_viewable()) {
            assert!(started.elapsed() < STEP_WAIT, "the toplevel did not map");
            pump_for(Duration::from_millis(10));
        }
        pump_for(Duration::from_millis(500));
        window
    }

    /// Places a child over a white toplevel, paces presents on its frame callbacks, hides
    /// it, sends output while hidden, shows it, then unmaps and destroys the toplevel with
    /// the wgpu surface still alive. Under NATIVE_ROW_DIR the farm takes a screenshot at each
    /// step; the row checks pacing and present times. Run 3 rule: never present while
    /// hidden, and never present before the last present's frame callback.
    #[test]
    #[ignore = "needs a display: the farm runs it under Xvfb, weston and sway"]
    fn a_child_places_hides_shows_and_presents_only_on_its_frame_callback() {
        let window = white_toplevel();
        let placed = Rect {
            x: 100,
            y: 120,
            w: 400,
            h: 240,
        };
        let mut child = Child::new(&window, placed).expect("a child surface");
        let (width, height) = child.size();
        let (family, source) = font_for("JetBrains Mono").expect("built in");
        let style = Style {
            family,
            source,
            font_px: 13.0,
            blink: false,
            theme: Theme {
                background: GREEN,
                ..Theme::default()
            },
        };
        let mut shared = Shared::new(child.scale() as f32, &style).expect("a GPU");
        let (display, handle) = child.handles();
        let mut surface =
            unsafe { shared.surface_raw(display, handle, width, height) }.expect("a wgpu surface");
        let frame = Frame {
            rows: Vec::new(),
            keys: Vec::new(),
            fills: Vec::new(),
            cursor: None,
        };

        let mut presents = Vec::with_capacity(FRAMES);
        let mut waits = Vec::with_capacity(FRAMES);
        while presents.len() < FRAMES {
            waits.push(wait_ready(&mut child));
            presents.push(present(&mut child, &mut shared, &mut surface, &frame));
            child.changed();
        }
        pump_for(Duration::from_millis(200));
        checkpoint("placed", &child);

        child.hide();
        pump_for(Duration::from_millis(100));
        child.changed();
        let hidden = Instant::now();
        while hidden.elapsed() < Duration::from_millis(300) {
            pump();
            assert!(!child.ready(), "a hidden child never presents");
            std::thread::sleep(Duration::from_millis(1));
        }
        checkpoint("hidden", &child);

        child.show();
        let shown_wait = wait_ready(&mut child);
        let shown_present = present(&mut child, &mut shared, &mut surface, &frame);
        pump_for(Duration::from_millis(200));
        checkpoint("shown", &child);

        window.hide();
        pump_for(Duration::from_millis(200));
        child.changed();
        assert!(
            !child.ready(),
            "a child whose toplevel unmapped never presents"
        );
        unsafe { window.destroy() };
        pump_for(Duration::from_millis(200));
        drop(surface);
        drop(child);
        drop(shared);
        pump_for(Duration::from_millis(100));

        let worst = presents.iter().copied().fold(shown_present, f64::max);
        let report = json!({
            "present_ms": stats(&presents),
            "wait_ms": stats(&waits),
            "shown_wait_ms": shown_wait,
            "shown_present_ms": shown_present,
        });
        if let Some(dir) = row_dir() {
            std::fs::write(dir.join("report.json"), format!("{report}\n")).expect("report file");
        }
        assert!(
            worst < PRESENT_MS_MAX,
            "a present took {worst:.2} ms: {report}"
        );
    }
}
