use super::super::event::{MouseKind, ViewEvent};
use super::place::SCALE_KEYS;

/// The most clicks one press counts, as the macOS view clamps a click count: three picks a
/// line, and a quicker fourth press still picks a line.
pub const CLICKS_MAX: u8 = 3;

/// The lines-per-notch setting that asks for a page a notch (winuser.h `WHEEL_PAGESCROLL`,
/// which windows-sys does not carry).
pub const WHEEL_PAGESCROLL: u32 = u32::MAX;

/// The wheel delta of one notch (winuser.h `WHEEL_DELTA`).
const NOTCH_DELTA: i64 = 120;

/// The most lines one notch scrolls, a bound on the system setting and on a page.
const NOTCH_LINES_MAX: i64 = 1_000;

/// The system's double-click limits: the most milliseconds from one press to the next, and
/// the box in pixels, centred on the last press, the next press must land in
/// (`GetDoubleClickTime`, `SM_CXDOUBLECLK`, `SM_CYDOUBLECLK`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub time_ms: u32,
    pub box_px: (i32, i32),
}

/// How far one wheel notch scrolls, from `SPI_GETWHEELSCROLLLINES`: a number of lines, or a
/// page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notch {
    Lines(u32),
    Page,
}

/// The left-button drag that holds the view's mouse capture: the Shift of its press and the
/// last point it sent.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    option: bool,
    point: (f64, f64),
}

/// The mouse state of one view: the last left press with its time and pixels and the run of
/// clicks it counts, the part of a line the wheel carries in 120ths of a line, and the drag
/// under capture.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mouse {
    last: Option<(u32, (i32, i32))>,
    clicks: u8,
    carry: i32,
    drag: Option<Drag>,
}

impl Mouse {
    /// A left press: counts on from the last press when it comes within the time and the
    /// box, else starts at one, and opens the drag. Inputs: the message time, the client
    /// pixels, the point, whether Shift is held and the limits. Output: the press.
    pub fn press(
        &mut self,
        time_ms: u32,
        px: (i32, i32),
        point: (f64, f64),
        option: bool,
        limits: Limits,
    ) -> ViewEvent {
        let near = self.last.is_some_and(|(time, at)| {
            let soon = time_ms.wrapping_sub(time) <= limits.time_ms;
            let across = (px.0 - at.0).abs() <= limits.box_px.0 / 2;
            soon && across && (px.1 - at.1).abs() <= limits.box_px.1 / 2
        });
        self.clicks = if near {
            self.clicks.saturating_add(1).min(CLICKS_MAX)
        } else {
            1
        };
        self.last = Some((time_ms, px));
        self.drag = Some(Drag { option, point });
        self.event(MouseKind::Press, point, option)
    }

    /// A move: under a drag, to a point it has not sent yet, the drag; else nothing. Inputs:
    /// the point and whether Shift is held. Output: the event, if any.
    pub fn moved(&mut self, point: (f64, f64), option: bool) -> Option<ViewEvent> {
        let drag = self.drag.as_mut().filter(|drag| drag.point != point)?;
        drag.point = point;
        Some(self.event(MouseKind::Drag, point, option))
    }

    /// The left button going up: under a drag, ends it with the release; else nothing, as
    /// for a press another window took. Inputs: the point and whether Shift is held.
    /// Output: the event, if any.
    pub fn release(&mut self, point: (f64, f64), option: bool) -> Option<ViewEvent> {
        self.drag.take()?;
        Some(self.event(MouseKind::Release, point, option))
    }

    /// The capture lost to another window: ends a drag with its release at the last point
    /// it sent, with its press's Shift. Output: the event, or none with no drag.
    pub fn lost(&mut self) -> Option<ViewEvent> {
        let drag = self.drag.take()?;
        Some(self.event(MouseKind::Release, drag.point, drag.option))
    }

    /// A right press, which ends a run of left clicks, as the system's double click does.
    pub fn right(&mut self) {
        self.last = None;
    }

    /// One wheel message: adds its delta times the lines a notch to the carry, dropped first
    /// when the wheel turned the other way, and answers the whole lines (120ths) it reaches,
    /// the exact rest carried. A page notch scrolls the visible lines; a setting of none
    /// scrolls nothing. Inputs: the delta, the notch, the visible lines and the point.
    /// Output: the wheel, or none below a line.
    pub fn wheel(
        &mut self,
        delta: i16,
        notch: Notch,
        page_lines: usize,
        point: (f64, f64),
    ) -> Option<ViewEvent> {
        let per = match notch {
            Notch::Lines(lines) => i64::from(lines),
            Notch::Page => i64::try_from(page_lines).unwrap_or(NOTCH_LINES_MAX),
        };
        let per = per.min(NOTCH_LINES_MAX);
        let turned = (self.carry > 0 && delta < 0) || (self.carry < 0 && delta > 0);
        if per == 0 {
            self.carry = 0;
            return None;
        }
        let total = if turned { 0 } else { i64::from(self.carry) } + i64::from(delta) * per;
        let lines = total / NOTCH_DELTA;
        self.carry = (total - lines * NOTCH_DELTA) as i32;
        let lines = lines.clamp(-NOTCH_LINES_MAX, NOTCH_LINES_MAX);
        (lines != 0).then_some(ViewEvent::Wheel {
            point,
            delta: lines as f64,
            precise: false,
        })
    }

    /// One mouse event of this view's run of clicks. Inputs: the kind, the point and
    /// whether Shift is held. Output: the event.
    fn event(&self, kind: MouseKind, point: (f64, f64), option: bool) -> ViewEvent {
        ViewEvent::Mouse {
            kind,
            point,
            option,
            clicks: self.clicks,
        }
    }
}

/// The client pixels a mouse message carries in its lParam, signed as GET_X_LPARAM and
/// GET_Y_LPARAM read them, so a point left of or above the view stays negative. Input: the
/// lParam. Output: x and y.
pub fn client_px(lparam: isize) -> (i32, i32) {
    let x = lparam as u16 as i16;
    let y = (lparam >> 16) as u16 as i16;
    (i32::from(x), i32::from(y))
}

/// Where a pixel sits in the view's own points, as the host reads every view's events:
/// pixels over the scale. Inputs: x and y in pixels and the scale key. Output: the point.
pub fn point(px: (i32, i32), key: u32) -> (f64, f64) {
    assert!(
        SCALE_KEYS.contains(&key),
        "a scale key of 100 to 400, got {key}"
    );
    let scale = f64::from(key) / 100.0;
    (f64::from(px.0) / scale, f64::from(px.1) / scale)
}

/// The signed delta in a WM_MOUSEWHEEL wParam's high word (GET_WHEEL_DELTA_WPARAM); its
/// low word is the key state. Input: the wParam. Output: the delta, positive away from the
/// user, which scrolls up into the history.
pub fn wheel_delta(wparam: usize) -> i16 {
    (wparam >> 16) as u16 as i16
}

/// How far a lines-per-notch setting scrolls. Input: the setting. Output: the notch.
pub fn notch(setting: u32) -> Notch {
    if setting == WHEEL_PAGESCROLL {
        Notch::Page
    } else {
        Notch::Lines(setting)
    }
}

/// Whether a WM_CONTEXTMENU lParam comes from the keyboard (Shift+F10 or the menu key),
/// which sends -1 in place of a screen point. Input: the lParam. Output: the answer.
pub fn keyboard_menu(lparam: isize) -> bool {
    lparam == -1
}

/// Where a keyboard menu opens: under the cursor cell, at its left edge, in the view's
/// points; at the view's corner with no cursor or no usable cell size. Inputs: the cursor's
/// row and column and the cell size in points. Output: the point.
pub fn cursor_point(cursor: Option<(usize, usize)>, cell: Option<(f64, f64)>) -> (f64, f64) {
    let sized = cell.filter(|(w, h)| w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0);
    let (Some((row, column)), Some((width, height))) = (cursor, sized) else {
        return (0.0, 0.0);
    };
    (column as f64 * width, row.saturating_add(1) as f64 * height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        time_ms: 500,
        box_px: (4, 4),
    };

    fn clicks(event: ViewEvent) -> u8 {
        match event {
            ViewEvent::Mouse { clicks, .. } => clicks,
            other => panic!("a mouse event, got {other:?}"),
        }
    }

    fn mouse(kind: MouseKind, point: (f64, f64), option: bool, clicks: u8) -> ViewEvent {
        ViewEvent::Mouse {
            kind,
            point,
            option,
            clicks,
        }
    }

    fn lines(delta: f64) -> Option<ViewEvent> {
        Some(ViewEvent::Wheel {
            point: (1.0, 2.0),
            delta,
            precise: false,
        })
    }

    #[test]
    fn a_press_inside_the_time_and_box_counts_on_up_to_three() {
        let mut state = Mouse::default();
        let at = (0.0, 0.0);
        assert_eq!(clicks(state.press(0, (10, 10), at, false, LIMITS)), 1);
        let second = clicks(state.press(100, (12, 8), at, false, LIMITS));
        assert_eq!(second, 2, "the second press inside the limits counts 2");
        assert_eq!(clicks(state.press(600, (10, 10), at, false, LIMITS)), 3);
        assert_eq!(
            clicks(state.press(700, (10, 10), at, false, LIMITS)),
            3,
            "capped"
        );
    }

    #[test]
    fn a_press_late_out_of_the_box_or_after_a_right_press_starts_again() {
        let mut state = Mouse::default();
        let at = (0.0, 0.0);
        assert_eq!(clicks(state.press(0, (10, 10), at, false, LIMITS)), 1);
        assert_eq!(
            clicks(state.press(501, (10, 10), at, false, LIMITS)),
            1,
            "late"
        );
        assert_eq!(
            clicks(state.press(600, (13, 10), at, false, LIMITS)),
            1,
            "across"
        );
        assert_eq!(
            clicks(state.press(700, (13, 13), at, false, LIMITS)),
            1,
            "down"
        );
        assert_eq!(clicks(state.press(800, (13, 13), at, false, LIMITS)), 2);
        state.right();
        assert_eq!(
            clicks(state.press(900, (13, 13), at, false, LIMITS)),
            1,
            "right"
        );
    }

    #[test]
    fn a_double_click_across_the_message_clock_wrapping_still_counts() {
        let mut state = Mouse::default();
        let at = (0.0, 0.0);
        assert_eq!(
            clicks(state.press(u32::MAX - 50, (5, 5), at, false, LIMITS)),
            1
        );
        assert_eq!(clicks(state.press(49, (5, 5), at, false, LIMITS)), 2);
    }

    #[test]
    fn a_drag_sends_each_new_point_once_and_the_release_ends_it() {
        let mut state = Mouse::default();
        let (start, next) = ((1.0, 1.0), (9.0, 1.0));
        assert_eq!(state.moved(next, false), None, "no drag before a press");
        let press = state.press(0, (1, 1), start, true, LIMITS);
        assert_eq!(press, mouse(MouseKind::Press, start, true, 1));
        assert_eq!(state.moved(start, true), None, "the pressed point again");
        let drag = state.moved(next, false);
        assert_eq!(drag, Some(mouse(MouseKind::Drag, next, false, 1)));
        assert_eq!(state.moved(next, false), None, "the same point twice");
        let release = state.release(next, false);
        assert_eq!(release, Some(mouse(MouseKind::Release, next, false, 1)));
        assert_eq!(state.moved((20.0, 1.0), false), None, "the drag ended");
        assert_eq!(state.release(next, false), None, "one release a press");
        assert_eq!(state.lost(), None, "no drag to end");
    }

    #[test]
    fn a_lost_capture_releases_at_the_last_point_with_the_press_shift() {
        let mut state = Mouse::default();
        let (start, next) = ((1.0, 1.0), (4.0, 3.0));
        let _ = state.press(0, (1, 1), start, true, LIMITS);
        assert!(state.moved(next, false).is_some());
        let lost = state.lost();
        assert_eq!(lost, Some(mouse(MouseKind::Release, next, true, 1)));
        assert_eq!(state.lost(), None, "one release a drag");
        assert_eq!(state.release(next, false), None, "the drag ended");
    }

    #[test]
    fn a_notch_scrolls_the_system_lines_and_touchpad_deltas_build_up() {
        let mut state = Mouse::default();
        let at = (1.0, 2.0);
        assert_eq!(
            state.wheel(120, Notch::Lines(3), 24, at),
            lines(3.0),
            "one notch up"
        );
        assert_eq!(state.wheel(-120, Notch::Lines(3), 24, at), lines(-3.0));
        assert_eq!(state.wheel(40, Notch::Lines(3), 24, at), lines(1.0));
        assert_eq!(
            state.wheel(20, Notch::Lines(3), 24, at),
            None,
            "below a line"
        );
        assert_eq!(
            state.wheel(20, Notch::Lines(3), 24, at),
            lines(1.0),
            "built up"
        );
        assert_eq!(
            state.wheel(120, Notch::Lines(0), 24, at),
            None,
            "set to none"
        );
        assert_eq!(state.wheel(120, Notch::Page, 24, at), lines(24.0), "a page");
        assert_eq!(state.wheel(-120, Notch::Page, 0, at), None, "no frame yet");
        let most = state.wheel(i16::MAX, Notch::Lines(u32::MAX - 1), 24, at);
        assert_eq!(most, lines(1_000.0), "bounded");
    }

    #[test]
    fn small_deltas_add_up_to_exactly_the_lines_of_a_notch() {
        for per in [7, 100] {
            let mut state = Mouse::default();
            let mut scrolled = 0.0;
            for _ in 0..NOTCH_DELTA {
                if let Some(ViewEvent::Wheel { delta, .. }) =
                    state.wheel(1, Notch::Lines(per), 24, (1.0, 2.0))
                {
                    scrolled += delta;
                }
            }
            let want = f64::from(per);
            assert_eq!(
                scrolled, want,
                "a notch in deltas of 1 at {per} lines a notch"
            );
        }
    }

    #[test]
    fn a_wheel_turned_the_other_way_drops_the_carry_first() {
        let mut state = Mouse::default();
        let at = (1.0, 2.0);
        assert_eq!(state.wheel(20, Notch::Lines(3), 24, at), None);
        assert_eq!(state.wheel(-20, Notch::Lines(3), 24, at), None);
        let back = state.wheel(-20, Notch::Lines(3), 24, at);
        assert_eq!(back, lines(-1.0), "the carry up was dropped, not summed");
    }

    #[test]
    fn client_pixels_are_signed_and_points_are_pixels_over_the_scale() {
        assert_eq!(client_px(0x0014_000A), (10, 20));
        let packed = ((-7i16 as u16 as isize) << 16) | (-5i16 as u16 as isize);
        assert_eq!(client_px(packed), (-5, -7), "left of and above the view");
        assert_eq!(point((30, 45), 150), (20.0, 30.0));
        assert_eq!(point((10, -20), 100), (10.0, -20.0));
        assert_eq!(wheel_delta(0xFF88_0000), -120, "towards the user");
        assert_eq!(
            wheel_delta(0x0078_0004),
            120,
            "the key state is the low word"
        );
        assert_eq!(notch(3), Notch::Lines(3));
        assert_eq!(notch(WHEEL_PAGESCROLL), Notch::Page);
    }

    #[test]
    #[should_panic(expected = "a scale key of 100 to 400")]
    fn a_point_at_a_scale_key_outside_100_to_400_is_a_bug() {
        let _ = point((1, 1), 99);
    }

    #[test]
    fn a_keyboard_menu_opens_under_the_cursor_cell_and_a_mouse_menu_is_a_point() {
        assert!(keyboard_menu(-1));
        assert!(!keyboard_menu(0xFFFF_FFFF), "a screen point of -1, -1");
        assert!(!keyboard_menu(0x0014_000A));
        let cell = Some((8.0, 17.0));
        assert_eq!(cursor_point(Some((2, 5)), cell), (40.0, 51.0));
        assert_eq!(cursor_point(None, cell), (0.0, 0.0), "no cursor");
        assert_eq!(cursor_point(Some((2, 5)), None), (0.0, 0.0), "no cell yet");
        assert_eq!(cursor_point(Some((2, 5)), Some((0.0, 17.0))), (0.0, 0.0));
        assert_eq!(
            cursor_point(Some((2, 5)), Some((f64::NAN, 17.0))),
            (0.0, 0.0)
        );
    }
}
