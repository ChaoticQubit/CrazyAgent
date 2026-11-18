/// The most bytes one `send-keys -H` command types, as the engine sends input.
pub const SEND_KEYS_BYTES_MAX: usize = 256;

/// The most bytes one copy may put on the Mac pasteboard.
pub const COPY_BYTES_MAX: usize = 1 << 20;

/// The bracketed-paste end marker; a paste carrying it could end the bracket early.
const PASTE_END: &[u8] = b"\x1b[201~";

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
#[cfg_attr(not(test), allow(dead_code))]
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

/// Whether a target is a plain session id such as `$3`. Input: the target. Output: answer.
pub fn session_id_valid(target: &str) -> bool {
    let digits = target.strip_prefix('$').unwrap_or("");
    !digits.is_empty() && digits.len() <= 9 && digits.bytes().all(|b| b.is_ascii_digit())
}

/// The `send-keys -H` commands that type the bytes into a pane, at most
/// `SEND_KEYS_BYTES_MAX` bytes each, every one ending in a newline. Inputs: the pane id and
/// the bytes. Output: the command lines, none for no bytes.
pub fn send_keys_lines(pane: &str, bytes: &[u8]) -> Vec<String> {
    assert!(pane_id_valid(pane));
    let lines: Vec<String> = bytes
        .chunks(SEND_KEYS_BYTES_MAX)
        .map(|run| {
            let mut line = String::with_capacity(20 + pane.len() + run.len() * 3);
            line.push_str("send-keys -t ");
            line.push_str(pane);
            line.push_str(" -H");
            for byte in run {
                line.push_str(&format!(" {byte:02x}"));
            }
            line.push('\n');
            assert_eq!(
                line.len(),
                "send-keys -t  -H\n".len() + pane.len() + run.len() * 3
            );
            line
        })
        .collect();
    assert_eq!(lines.len(), bytes.len().div_ceil(SEND_KEYS_BYTES_MAX));
    lines
}

/// Reads one `send-keys -t <pane> -H xx ...` line back into its pane and bytes, the inverse
/// of `send_keys_lines` for one line. Input: the line with or without its newline. Output:
/// the pane and bytes, or none for any other line.
#[cfg_attr(not(test), allow(dead_code))]
pub fn send_keys_decode(line: &str) -> Option<(String, Vec<u8>)> {
    let rest = line.trim_end_matches('\n').strip_prefix("send-keys -t ")?;
    let (pane, hex) = rest.split_once(" -H")?;
    if !pane_id_valid(pane) {
        return None;
    }
    let bytes: Option<Vec<u8>> = hex
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| {
            if w.len() == 2 {
                u8::from_str_radix(w, 16).ok()
            } else {
                None
            }
        })
        .collect();
    let bytes = bytes?;
    (bytes.len() <= SEND_KEYS_BYTES_MAX).then(|| (pane.to_string(), bytes))
}

/// The bytes a paste types: CR LF and LF become CR, as terminals send Enter, and the
/// bracketed-paste end marker is always removed from the text, since tmux's `paste-buffer -p`
/// may wrap it; with `bracketed` the whole is wrapped here. Inputs: the pasted text and
/// whether to wrap it. Output: the bytes.
pub fn paste_bytes(text: &[u8], bracketed: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(text.len() + 12);
    let mut at = 0;
    while at < text.len() {
        if text[at..].starts_with(PASTE_END) {
            at += PASTE_END.len();
            continue;
        }
        match (text[at], text.get(at + 1)) {
            (b'\r', Some(b'\n')) => {
                body.push(b'\r');
                at += 2;
                continue;
            }
            (b'\n', _) => body.push(b'\r'),
            (byte, _) => body.push(byte),
        }
        at += 1;
    }
    if bracketed {
        let mut wrapped = b"\x1b[200~".to_vec();
        wrapped.extend_from_slice(&body);
        wrapped.extend_from_slice(PASTE_END);
        body = wrapped;
    }
    assert!(!body.contains(&b'\n'));
    body
}

/// Splits paste bytes into the runs one `send-keys -H` command each types, in order. Input:
/// the bytes. Output: runs of at most `SEND_KEYS_BYTES_MAX` bytes whose concatenation is the
/// input.
pub fn paste_runs(bytes: &[u8]) -> Vec<Vec<u8>> {
    let runs: Vec<Vec<u8>> = bytes
        .chunks(SEND_KEYS_BYTES_MAX)
        .map(<[u8]>::to_vec)
        .collect();
    assert_eq!(runs.iter().map(Vec::len).sum::<usize>(), bytes.len());
    runs
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

/// The `set-buffer` command that loads paste bytes into a named tmux buffer: every byte
/// except ASCII letters and digits is written as `\ooo` inside double quotes, so no tmux
/// expansion or quoting applies (h3's T7 rule). Inputs: the buffer name and the bytes, with no
/// NUL. Output: the command line.
pub fn set_buffer_line(name: &str, bytes: &[u8]) -> String {
    assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    assert!(
        !bytes.contains(&0),
        "tmux buffers cannot take NUL through a command"
    );
    let mut line = String::with_capacity(bytes.len() * 2 + name.len() + 32);
    line.push_str("set-buffer -b ");
    line.push_str(name);
    line.push_str(" -- \"");
    for byte in bytes {
        if byte.is_ascii_alphanumeric() {
            line.push(char::from(*byte));
        } else {
            line.push_str(&format!("\\{byte:03o}"));
        }
    }
    line.push_str("\"\n");
    assert_eq!(line.matches('\n').count(), 1);
    line
}

/// The `paste-buffer` command that writes a named buffer to a pane and deletes it: `-p`
/// lets tmux add the bracketed-paste markers only when the pane asked for them, `-r` keeps
/// the bytes as given, `-S` passes control bytes unchanged.
/// Inputs: the buffer name and the pane id. Output: the command line.
pub fn paste_buffer_line(name: &str, pane: &str) -> String {
    assert!(pane_id_valid(pane));
    assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    format!("paste-buffer -d -p -r -S -b {name} -t {pane}\n")
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

/// The cell under a window point, clamped to the grid. Inputs: the point in physical
/// pixels, the cell size, the padding and the grid size. Output: 0-based column and line.
pub fn cell_at(
    x: f64,
    y: f64,
    cell: (f64, f64),
    pad: f64,
    columns: usize,
    lines: usize,
) -> (usize, usize) {
    assert!(cell.0 > 0.0 && cell.1 > 0.0 && columns > 0 && lines > 0);
    let column = ((x - pad) / cell.0)
        .floor()
        .clamp(0.0, (columns - 1) as f64) as usize;
    let line = ((y - pad) / cell.1).floor().clamp(0.0, (lines - 1) as f64) as usize;
    assert!(column < columns && line < lines);
    (column, line)
}

/// The grid that fits a window size, at least 2 by 2 and at most `limit` per side. Inputs:
/// the size in physical pixels, the cell size, the padding and the limit. Output: columns
/// and lines.
pub fn grid_for(
    width: u32,
    height: u32,
    cell: (f64, f64),
    pad: f64,
    limit: usize,
) -> (usize, usize) {
    assert!(cell.0 > 0.0 && cell.1 > 0.0 && limit >= 2);
    let fit = |px: u32, size: f64| {
        ((f64::from(px) - 2.0 * pad) / size)
            .floor()
            .clamp(2.0, limit as f64) as usize
    };
    let grid = (fit(width, cell.0), fit(height, cell.1));
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
    fn send_keys_encode_decode_round_trip() {
        assert_eq!(
            send_keys_lines("%3", b"hi\r"),
            vec!["send-keys -t %3 -H 68 69 0d\n"]
        );
        assert!(send_keys_lines("%3", b"").is_empty());
        let long: Vec<u8> = (0..=255u8)
            .cycle()
            .take(SEND_KEYS_BYTES_MAX * 2 + 1)
            .collect();
        let lines = send_keys_lines("%10", &long);
        assert_eq!(lines.len(), 3);
        let back: Vec<u8> = lines
            .iter()
            .flat_map(|l| send_keys_decode(l).unwrap().1)
            .collect();
        assert_eq!(back, long);
        assert!(send_keys_decode("kill-server").is_none());
        assert!(send_keys_decode("send-keys -t %1;x -H 41").is_none());
    }

    #[test]
    fn ids() {
        assert!(pane_id_valid("%0"));
        assert!(!pane_id_valid("%"));
        assert!(!pane_id_valid("%1;kill-server"));
        assert!(session_id_valid("$0"));
        assert!(!session_id_valid("main"));
    }

    #[test]
    fn paste_normalizes_and_brackets() {
        assert_eq!(paste_bytes(b"a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes(b"x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
        assert_eq!(paste_bytes(b"x\x1b[201~y", false), b"xy");
        let runs = paste_runs(&[7u8; 600]);
        assert_eq!(
            runs.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![256, 256, 88]
        );
        assert!(paste_runs(b"").is_empty());
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
        assert_eq!(cell_at(-5.0, 1e9, (10.0, 20.0), 4.0, 80, 24), (0, 23));
        assert_eq!(grid_for(808, 488, (10.0, 20.0), 4.0, 1000), (80, 24));
        assert_eq!(grid_for(1, 1, (10.0, 20.0), 4.0, 1000), (2, 2));
    }

    #[test]
    fn set_buffer_quotes_every_byte() {
        let all: Vec<u8> = (1..=255u8).collect();
        let line = set_buffer_line("ctm-1-1", &all);
        let body = line
            .strip_prefix("set-buffer -b ctm-1-1 -- \"")
            .unwrap()
            .strip_suffix("\"\n")
            .unwrap();
        assert!(
            body.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'\\')
        );
        assert_eq!(unescape(body.as_bytes()), all);
        assert_eq!(
            paste_buffer_line("ctm-1-1", "%3"),
            "paste-buffer -d -p -r -S -b ctm-1-1 -t %3\n"
        );
    }

    #[test]
    fn sgr_buttons() {
        assert_eq!(sgr_button(0, 2, 3, true), b"\x1b[<0;3;4M");
        assert_eq!(sgr_button(0, 2, 3, false), b"\x1b[<0;3;4m");
        assert_eq!(sgr_button(32, 0, 0, true), b"\x1b[<32;1;1M");
    }
}
