/// The most bytes one copy may put on the clipboard.
pub const COPY_BYTES_MAX: usize = 1 << 20;

/// Decodes a `%output` payload: tmux writes each byte below 32 and the backslash as a
/// backslash and three octal digits. Input: the escaped payload. Output: the raw bytes; a
/// backslash not followed by three octal digits stays as it is.
pub fn unescape(escaped: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(escaped.len());
    let mut at = 0;
    while at < escaped.len() {
        let byte = escaped[at];
        let digits = escaped.get(at + 1..at + 4);
        let octal = digits.filter(|d| d.iter().all(|b| (b'0'..=b'7').contains(b)));
        if byte == b'\\' {
            if let Some(d) = octal {
                let value = u32::from(d[0] - b'0') * 64
                    + u32::from(d[1] - b'0') * 8
                    + u32::from(d[2] - b'0');
                if let Ok(decoded) = u8::try_from(value) {
                    out.push(decoded);
                    at += 4;
                    continue;
                }
            }
        }
        out.push(byte);
        at += 1;
    }
    assert!(out.len() <= escaped.len());
    out
}

/// Encodes raw bytes the way tmux writes a `%output` payload, the inverse of `unescape`.
/// Input: raw bytes. Output: the escaped payload.
#[cfg(test)]
pub fn escape(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() * 2);
    for byte in raw {
        if *byte < 32 || *byte == b'\\' {
            out.extend_from_slice(format!("\\{byte:03o}").as_bytes());
        } else {
            out.push(*byte);
        }
    }
    assert!(out.len() >= raw.len());
    out
}

/// Whether a target is a plain pane id such as `%12`. Input: the target. Output: the answer.
pub fn pane_id_valid(target: &str) -> bool {
    let digits = target.strip_prefix('%').unwrap_or("");
    !digits.is_empty() && digits.len() <= 9 && digits.bytes().all(|b| b.is_ascii_digit())
}

/// The scrollback offset after a wheel move, kept inside the history. Inputs: the offset
/// (0 = live screen), the move in lines (up is positive) and the history length. Output: the
/// new offset, between 0 and the history length.
pub fn scroll_clamp(offset: usize, delta: i32, history: usize) -> usize {
    let wanted = offset as i64 + i64::from(delta);
    let clamped = wanted.clamp(0, history as i64) as usize;
    assert!(clamped <= history);
    clamped
}

/// One SGR mouse wheel report for a cell, 1-based as terminals send it. Inputs: whether the
/// wheel moved up, and the 0-based column and line. Output: the report bytes.
pub fn sgr_wheel(up: bool, column: usize, line: usize) -> Vec<u8> {
    let button = if up { 64 } else { 65 };
    let report = format!("\x1b[<{button};{};{}M", column + 1, line + 1).into_bytes();
    assert!(report.ends_with(b"M"));
    report
}

/// One SGR mouse button report, 1-based. Inputs: the button code (0 left, 32 added for a
/// drag), the 0-based column and line, and whether it is a press (else a release). Output:
/// the report bytes.
pub fn sgr_button(code: u8, column: usize, line: usize, press: bool) -> Vec<u8> {
    assert!(code < 128);
    let end = if press { 'M' } else { 'm' };
    let report = format!("\x1b[<{code};{};{}{end}", column + 1, line + 1).into_bytes();
    assert!(report.starts_with(b"\x1b[<"));
    report
}

/// Whole wheel lines from a pixel delta plus the carry left from earlier deltas. Inputs: the
/// pixel delta (up is positive), the carry and the line height in pixels. Output: the lines
/// to move and the new carry, whose size stays below one line.
pub fn wheel_lines(pixels: f64, carry: f64, line_px: f64) -> (i32, f64) {
    assert!(line_px > 0.0);
    let total = (carry + pixels).clamp(-1e6, 1e6);
    let lines = (total / line_px).trunc();
    let left = total - lines * line_px;
    assert!(left.abs() < line_px);
    (lines as i32, left)
}

/// The width today's xterm panel keeps for its scrollbar when it fits a grid, in CSS
/// pixels; the native view keeps the same gutter on the right, so one host rect gives one
/// grid in both.
pub const SCROLLBAR_PX: f64 = 14.0;

/// The cell under a view point, clamped to the grid. Inputs: the point in the view's
/// points (CSS pixels), the cell size in CSS pixels and the grid size. Output: 0-based
/// column and line.
pub fn cell_at(x: f64, y: f64, cell: (f64, f64), columns: usize, lines: usize) -> (usize, usize) {
    assert!(cell.0 > 0.0 && cell.1 > 0.0 && columns > 0 && lines > 0);
    let column = (x / cell.0).floor().clamp(0.0, (columns - 1) as f64) as usize;
    let line = (y / cell.1).floor().clamp(0.0, (lines - 1) as f64) as usize;
    assert!(column < columns && line < lines);
    (column, line)
}

/// The grid that fits a host rect as xterm's fit does: whole CSS pixels, less the
/// scrollbar gutter across, at least 2 by 2 and at most `limit` per side. Inputs: the rect
/// size and the cell size in CSS pixels and the limit. Output: columns and lines.
pub fn grid_for(width: f64, height: f64, cell: (f64, f64), limit: usize) -> (usize, usize) {
    assert!(cell.0 > 0.0 && cell.1 > 0.0 && limit >= 2);
    assert!(width.is_finite() && height.is_finite());
    let fit = |px: f64, size: f64| (px / size).floor().clamp(2.0, limit as f64) as usize;
    let grid = (
        fit(width.floor() - SCROLLBAR_PX, cell.0),
        fit(height.floor(), cell.1),
    );
    assert!(grid.0 >= 2 && grid.1 >= 2);
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_decodes_octal_and_keeps_text() {
        assert_eq!(
            unescape(br"e\010echo z\033[?2004l\015\015\012"),
            b"e\x08echo z\x1b[?2004l\r\r\n"
        );
        assert_eq!(unescape(br"a\134b"), b"a\\b");
        assert_eq!(unescape(br"tail\01"), br"tail\01");
        assert_eq!(unescape(br"\999"), br"\999");
        assert_eq!(unescape(b""), b"");
    }

    #[test]
    fn escape_round_trips_every_byte() {
        let raw: Vec<u8> = (0..=255u8).collect();
        assert_eq!(unescape(&escape(&raw)), raw);
    }

    #[test]
    fn ids() {
        assert!(pane_id_valid("%0"));
        assert!(!pane_id_valid("%"));
        assert!(!pane_id_valid("%1;kill-server"));
        assert!(!pane_id_valid("%1234567890"));
    }

    #[test]
    fn scroll_and_cells() {
        assert_eq!(scroll_clamp(0, -3, 100), 0);
        assert_eq!(scroll_clamp(98, 5, 100), 100);
        assert_eq!(scroll_clamp(10, 3, 100), 13);
        assert_eq!(sgr_wheel(true, 0, 0), b"\x1b[<64;1;1M");
        assert_eq!(sgr_wheel(false, 9, 4), b"\x1b[<65;10;5M");
        assert_eq!(wheel_lines(25.0, 0.0, 10.0), (2, 5.0));
        assert_eq!(wheel_lines(-25.0, 0.0, 10.0), (-2, -5.0));
        assert_eq!(cell_at(-5.0, 1e9, (10.0, 20.0), 80, 24), (0, 23));
        assert_eq!(cell_at(15.0, 39.9, (7.5, 20.0), 80, 24), (2, 1));
        assert_eq!(grid_for(712.9, 357.0, (7.5, 17.0), 1000), (93, 21));
        assert_eq!(grid_for(711.0, 356.9, (7.5, 17.0), 1000), (92, 20));
        assert_eq!(grid_for(1.0, 1.0, (10.0, 20.0), 1000), (2, 2));
        assert_eq!(grid_for(1e9, 1e9, (10.0, 20.0), 1000), (1000, 1000));
    }

    #[test]
    fn sgr_buttons() {
        assert_eq!(sgr_button(0, 2, 3, true), b"\x1b[<0;3;4M");
        assert_eq!(sgr_button(0, 2, 3, false), b"\x1b[<0;3;4m");
        assert_eq!(sgr_button(32, 0, 0, true), b"\x1b[<32;1;1M");
    }
}
