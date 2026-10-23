/// Reads the local UTC offset from a compiled time zone file such as `/etc/localtime`.
/// Takes the TZif bytes and a moment in seconds since 1970.
/// Returns the offset in seconds, or none for a file that is not valid TZif.
pub(in crate::tui) fn tzif_offset(bytes: &[u8], at: i64) -> Option<i64> {
    let (header, version) = header_at(bytes, 0)?;
    let v1_size = block_size(&header, 4)?;
    if version >= b'2' {
        let start = 44usize.checked_add(v1_size)?;
        let (second, _) = header_at(bytes, start)?;
        return offset(bytes, start + 44, &second, 8, at);
    }
    offset(bytes, 44, &header, 4, at)
}

/// Gives the local UTC offset of this Mac at one moment.
/// Takes a moment in milliseconds since 1970; reads `/etc/localtime` once per call.
/// Returns the offset in seconds, or zero when the zone cannot be read.
pub(in crate::tui) fn local_offset(at_ms: i64) -> i64 {
    std::fs::read("/etc/localtime")
        .ok()
        .filter(|bytes| bytes.len() <= 1 << 20)
        .and_then(|bytes| tzif_offset(&bytes, at_ms.div_euclid(1000)))
        .unwrap_or(0)
}

/// Formats a moment as local 24-hour time.
/// Takes the moment in milliseconds and the UTC offset in seconds.
/// Returns text such as "10:38".
pub(in crate::tui) fn clock(at_ms: i64, offset: i64) -> String {
    let seconds = at_ms.div_euclid(1000) + offset;
    let minutes = seconds.div_euclid(60).rem_euclid(24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Formats a duration the short way the design uses.
/// Takes the duration in milliseconds; negative values count as zero.
/// Returns text such as "38 s", "19 min" or "2 h".
pub(in crate::tui) fn duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 3600 {
        format!("{} min", seconds / 60)
    } else if seconds < 48 * 3600 {
        format!("{} h", seconds / 3600)
    } else {
        format!("{} days", seconds / 86400)
    }
}

struct Counts {
    isut: usize,
    isstd: usize,
    leap: usize,
    time: usize,
    types: usize,
    chars: usize,
}

fn header_at(bytes: &[u8], at: usize) -> Option<(Counts, u8)> {
    let head = bytes.get(at..at.checked_add(44)?)?;
    if &head[..4] != b"TZif" {
        return None;
    }
    let count = |index: usize| -> Option<usize> {
        let start = 20 + index * 4;
        let raw = u32::from_be_bytes(head[start..start + 4].try_into().ok()?);
        usize::try_from(raw).ok().filter(|value| *value <= 1 << 16)
    };
    Some((
        Counts {
            isut: count(0)?,
            isstd: count(1)?,
            leap: count(2)?,
            time: count(3)?,
            types: count(4)?,
            chars: count(5)?,
        },
        head[4],
    ))
}

fn block_size(counts: &Counts, time_size: usize) -> Option<usize> {
    counts
        .time
        .checked_mul(time_size + 1)?
        .checked_add(counts.types.checked_mul(6)?)?
        .checked_add(counts.chars)?
        .checked_add(counts.leap.checked_mul(time_size + 4)?)?
        .checked_add(counts.isstd)?
        .checked_add(counts.isut)
}

fn offset(bytes: &[u8], start: usize, counts: &Counts, time_size: usize, at: i64) -> Option<i64> {
    if counts.types == 0 {
        return None;
    }
    let data = bytes.get(start..start.checked_add(block_size(counts, time_size)?)?)?;
    let time = |index: usize| -> i64 {
        let at = index * time_size;
        if time_size == 8 {
            i64::from_be_bytes(data[at..at + 8].try_into().unwrap_or([0; 8]))
        } else {
            i64::from(i32::from_be_bytes(
                data[at..at + 4].try_into().unwrap_or([0; 4]),
            ))
        }
    };
    let indices = counts.time * time_size;
    let infos = indices + counts.time;
    let mut kind = 0usize;
    for index in 0..counts.time {
        if time(index) > at {
            break;
        }
        kind = usize::from(data[indices + index]);
    }
    if kind >= counts.types {
        return None;
    }
    let info = infos + kind * 6;
    let utoff = i32::from_be_bytes(data[info..info + 4].try_into().ok()?);
    let utoff = i64::from(utoff);
    (utoff.abs() <= 26 * 3600).then_some(utoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tzif(version: u8, transitions: &[(i64, u8)], types: &[i32]) -> Vec<u8> {
        let header = |time: usize| {
            let mut head = b"TZif".to_vec();
            head.push(version);
            head.extend([0u8; 15]);
            for count in [0, 0, 0, transitions.len(), types.len(), 4] {
                head.extend(u32::try_from(count).unwrap().to_be_bytes());
            }
            let mut body = Vec::new();
            for (at, _) in transitions {
                if time == 8 {
                    body.extend(at.to_be_bytes());
                } else {
                    body.extend(i32::try_from(*at).unwrap().to_be_bytes());
                }
            }
            body.extend(transitions.iter().map(|(_, kind)| *kind));
            for offset in types {
                body.extend(offset.to_be_bytes());
                body.extend([0u8, 0u8]);
            }
            body.extend(b"UTC\0");
            head.extend(body);
            head
        };
        let mut bytes = header(4);
        if version >= b'2' {
            bytes.extend(header(8));
            bytes.extend(b"\n\n");
        }
        bytes
    }

    #[test]
    fn offsets_follow_the_last_transition_before_the_moment() {
        let file = tzif(b'2', &[(1_000, 1), (2_000, 0)], &[-25_200, -18_000]);
        assert_eq!(tzif_offset(&file, 500), Some(-25_200));
        assert_eq!(tzif_offset(&file, 1_500), Some(-18_000));
        assert_eq!(tzif_offset(&file, 9_999_999), Some(-25_200));
        let old = tzif(0, &[(0, 0)], &[19_800]);
        assert_eq!(tzif_offset(&old, 10), Some(19_800));
        assert_eq!(tzif_offset(b"nope", 0), None);
        let mut cut = file.clone();
        cut.truncate(60);
        assert_eq!(tzif_offset(&cut, 0), None);
        let bad_index = tzif(b'2', &[(0, 9)], &[0]);
        assert_eq!(tzif_offset(&bad_index, 10), None);
    }

    #[test]
    fn this_macs_zone_reads_as_a_whole_quarter_hour_offset() {
        let offset = local_offset(1_793_000_000_000);
        assert_eq!(offset % 900, 0);
        assert!(offset.abs() <= 14 * 3600);
    }

    #[test]
    fn clocks_and_durations_read_the_design_way() {
        assert_eq!(clock(0, 0), "00:00");
        assert_eq!(clock(38_280_000, 0), "10:38");
        assert_eq!(clock(38_280_000, -3600), "09:38");
        assert_eq!(clock(1_000, -7200), "22:00");
        assert_eq!(duration(-5), "0 s");
        assert_eq!(duration(38_400), "38 s");
        assert_eq!(duration(19 * 60_000 + 5_000), "19 min");
        assert_eq!(duration(3 * 3_600_000), "3 h");
        assert_eq!(duration(5 * 86_400_000), "5 days");
    }
}
