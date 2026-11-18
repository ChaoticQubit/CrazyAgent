use std::ops::RangeInclusive;

/// The largest device-pixel origin, edge or webview offset a native view is placed at.
pub const RECT_MAX: i32 = 32_768;

/// The plain code for a page rectangle or webview offset that cannot be placed.
pub const RECT_INVALID: &str = "native_terminal_rect_invalid";

/// The display scale keys a view draws at, in percent of 96 DPI: the render core's 1x to
/// 4x bounds.
pub const SCALE_KEYS: RangeInclusive<u32> = 100..=400;

/// A native view's rectangle in its parent window's client area, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// The scale key for a window's DPI: percent of 96, rounded, held to `SCALE_KEYS`. Input:
/// GetDpiForWindow's answer. Output: the key, or none for 0, which GetDpiForWindow returns
/// for a window it cannot read.
pub fn scale_key(dpi: u32) -> Option<u32> {
    let _ = dpi;
    unimplemented!("H1 red")
}

/// The device rectangle for the page's terminal box. Inputs: x, y, width and height in CSS
/// pixels, the scale key, and the webview's offset in the parent's client area in device
/// pixels. Output: the rectangle, edges rounded so boxes that touch on the page touch on
/// screen, or `RECT_INVALID` for a value that is not finite, out of bounds or under a pixel.
pub fn rect(
    page: (f64, f64, f64, f64),
    key: u32,
    offset: (i32, i32),
) -> Result<Rect, &'static str> {
    let _ = (page, key, offset);
    unimplemented!("H1 red")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dpi_gives_a_scale_key_held_to_one_to_four_times() {
        let keys = [
            (96, 100),
            (120, 125),
            (144, 150),
            (168, 175),
            (192, 200),
            (384, 400),
        ];
        for (dpi, key) in keys {
            assert_eq!(scale_key(dpi), Some(key), "dpi {dpi}");
        }
        assert_eq!(scale_key(72), Some(100));
        assert_eq!(scale_key(480), Some(400));
        assert_eq!(scale_key(u32::MAX), Some(400));
        assert_eq!(scale_key(0), None);
    }

    #[test]
    fn a_page_rect_is_finite_bounded_and_at_least_one_device_pixel() {
        let placed = rect((10.0, 20.0, 300.0, 200.0), 100, (0, 0));
        assert_eq!(
            placed,
            Ok(Rect {
                x: 10,
                y: 20,
                w: 300,
                h: 200
            })
        );
        let refused = [
            (f64::NAN, 0.0, 10.0, 10.0),
            (0.0, f64::INFINITY, 10.0, 10.0),
            (0.0, 0.0, f64::NEG_INFINITY, 10.0),
            (0.0, 0.0, 10.0, 0.0),
            (0.0, 0.0, 0.4, 10.0),
            (0.0, 0.0, -10.0, 10.0),
            (40_000.0, 0.0, 10.0, 10.0),
            (0.0, 0.0, 40_000.0, 10.0),
            (0.0, 0.0, 10.0, f64::MAX),
        ];
        for page in refused {
            assert_eq!(rect(page, 100, (0, 0)), Err(RECT_INVALID), "{page:?}");
        }
        assert_eq!(
            rect((0.0, 0.0, 20_000.0, 10.0), 200, (0, 0)),
            Err(RECT_INVALID)
        );
    }

    #[test]
    fn fractional_scales_round_edges_so_touching_boxes_touch() {
        for key in [125, 150, 175] {
            let left = rect((0.3, 0.0, 100.9, 50.0), key, (0, 0)).expect("left box");
            let right = rect((101.2, 0.0, 99.0, 50.0), key, (0, 0)).expect("right box");
            let edge = left.x + i32::try_from(left.w).expect("a width");
            assert_eq!(edge, right.x, "scale {key}");
            let scale = f64::from(key) / 100.0;
            assert_eq!(f64::from(right.x), (101.2 * scale).round(), "scale {key}");
            let far = (200.2 * scale).round();
            assert_eq!(f64::from(right.x) + f64::from(right.w), far, "scale {key}");
        }
        let at = rect((0.5, 0.5, 1.0, 1.0), 150, (0, 0)).expect("a small box");
        assert_eq!(
            at,
            Rect {
                x: 1,
                y: 1,
                w: 1,
                h: 1
            }
        );
    }

    #[test]
    fn the_webview_offset_moves_the_rect_and_is_bounded() {
        let placed = rect((10.0, 10.0, 100.0, 50.0), 150, (8, 31)).expect("an offset box");
        assert_eq!(
            placed,
            Rect {
                x: 23,
                y: 46,
                w: 150,
                h: 75
            }
        );
        let moved = rect((0.0, 0.0, 10.0, 10.0), 100, (-5, -5)).expect("a negative offset");
        assert_eq!((moved.x, moved.y), (-5, -5));
        for offset in [
            (RECT_MAX + 1, 0),
            (0, -RECT_MAX - 1),
            (i32::MIN, 0),
            (0, i32::MAX),
        ] {
            let page = (0.0, 0.0, 10.0, 10.0);
            assert_eq!(rect(page, 100, offset), Err(RECT_INVALID), "{offset:?}");
        }
    }

    #[test]
    #[should_panic(expected = "a scale key")]
    fn a_scale_key_out_of_range_is_a_bug() {
        let _ = rect((0.0, 0.0, 10.0, 10.0), 99, (0, 0));
    }
}
