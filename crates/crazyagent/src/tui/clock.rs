use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const OFFSET_SECONDS_MAX: i64 = 14 * 3600;

static OFFSET: OnceLock<i64> = OnceLock::new();

/// Parses a `date +%z` offset such as "+0200" or "-0530".
/// Input: the text. Output: seconds east of UTC, or None for anything else.
pub fn parse_offset(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    let offset = sign * (hours * 3600 + minutes * 60);
    (offset.abs() <= OFFSET_SECONDS_MAX && minutes < 60).then_some(offset)
}

/// The local UTC offset, read once with `/bin/date +%z`.
/// Input: none. Output: seconds east of UTC; 0 when the command fails.
pub fn local_offset() -> i64 {
    *OFFSET.get_or_init(|| {
        std::process::Command::new("/bin/date")
            .arg("+%z")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|text| parse_offset(&text))
            .unwrap_or(0)
    })
}

/// Milliseconds since the Unix epoch now.
/// Input: none. Output: the time.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Local 24-hour clock time, such as "10:42".
/// Inputs: epoch milliseconds and the UTC offset. Output: the time.
pub fn clock(ms: i64, offset: i64) -> String {
    let seconds = (ms.div_euclid(1000) + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60)
}

/// A short past time: "just now", "4 min ago", "today 09:12", "yesterday" or
/// "Oct 19".
/// Inputs: the time, now, both in epoch milliseconds, and the UTC offset.
/// Output: the plain phrase.
pub fn ago(ms: i64, now: i64, offset: i64) -> String {
    let elapsed = (now - ms).max(0) / 1000;
    if elapsed < 60 {
        return "just now".into();
    }
    if elapsed < 3600 {
        return format!("{} min ago", elapsed / 60);
    }
    let day = |at: i64| (at.div_euclid(1000) + offset).div_euclid(86_400);
    let (then, today) = (day(ms), day(now));
    if then == today {
        return format!("today {}", clock(ms, offset));
    }
    if then + 1 == today {
        return "yesterday".into();
    }
    let (_, month, date) = civil(then);
    format!("{} {date}", MONTHS[month as usize - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    const OCT_23_1042_UTC: i64 = 1_792_752_120_000;

    #[test]
    fn offsets_parse_only_valid_zones() {
        assert_eq!(parse_offset("+0200\n"), Some(7200));
        assert_eq!(parse_offset("-0530"), Some(-19_800));
        assert_eq!(parse_offset("0200"), None);
        assert_eq!(parse_offset("+02:00"), None);
        assert_eq!(parse_offset("+9900"), None);
    }

    #[test]
    fn times_read_like_the_design() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(clock(OCT_23_1042_UTC, 0), "10:42");
        assert_eq!(clock(OCT_23_1042_UTC, -19_800), "05:12");
        let now = OCT_23_1042_UTC;
        assert_eq!(ago(now - 30_000, now, 0), "just now");
        assert_eq!(ago(now - 4 * 60_000, now, 0), "4 min ago");
        assert_eq!(ago(now - 90 * 60_000, now, 0), "today 09:12");
        assert_eq!(ago(now - 24 * 3_600_000, now, 0), "yesterday");
        assert_eq!(ago(now - 4 * 86_400_000, now, 0), "Oct 19");
        assert_eq!(ago(now + 60_000, now, 0), "just now");
    }
}
