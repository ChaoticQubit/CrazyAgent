use super::power::PowerReading;
use super::state::{Heat, LidSupport, PowerSource};
use std::io::Read;
use std::path::{Path, PathBuf};

const ENTRIES_MAX: usize = 64;
const SCAN_MAX: usize = 4096;
const TRIPS_MAX: usize = 16;
const VALUE_BYTES_MAX: u64 = 64;
const PERCENT_MAX: u32 = 100;
const SUPPLIES: &str = "sys/class/power_supply";
const THERMAL: &str = "sys/class/thermal";
const LIDS: &str = "proc/acpi/button/lid";
const SEATS: &str = "run/systemd/seats";
const ZONE_PREFIX: &str = "thermal_zone";
const STATE_PREFIX: &str = "state:";

/// Reads one small sysfs value. Input: the file. Output: its text without the line end,
/// or none when it is missing, unreadable, not text, or longer than a sysfs value is.
fn value(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(VALUE_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > VALUE_BYTES_MAX {
        return None;
    }
    Some(text.trim().to_owned())
}

/// Lists the entries of one folder whose name starts with `prefix`: the first
/// `ENTRIES_MAX` of them in name order, out of at most `SCAN_MAX` entries looked at.
/// Inputs: the folder and the name start, empty for every name. Output: the entry paths,
/// empty when the folder cannot be read; a name that is not text is left out.
fn entries(folder: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = listing
        .take(SCAN_MAX)
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(prefix))
        })
        .map(|entry| entry.path())
        .collect();
    assert!(found.len() <= SCAN_MAX);
    found.sort();
    found.truncate(ENTRIES_MAX);
    assert!(found.len() <= ENTRIES_MAX);
    found
}

/// Reads the power supplies below a `/sys/class/power_supply` shaped folder. A system
/// battery (not a mouse's or keyboard's) that discharges means battery, whatever else is
/// attached. Else a supply online, a battery charging or full, or no system battery at
/// all means AC; a system battery that says nothing stays unknown. Input: the folder.
/// Output: the source and the mean charge of the system batteries, none where unread.
pub fn supplies(root: &Path) -> (PowerSource, Option<u8>) {
    let mut online = false;
    let mut battery = false;
    let mut discharging = false;
    let mut charging = false;
    let mut charges: Vec<u32> = Vec::new();
    for supply in entries(root, "") {
        let kind = value(&supply.join("type")).unwrap_or_default();
        if kind != "Battery" {
            let state = value(&supply.join("online")).and_then(|text| text.parse::<u32>().ok());
            online |= state.is_some_and(|state| state > 0);
            continue;
        }
        if value(&supply.join("scope")).as_deref() == Some("Device") {
            continue;
        }
        battery = true;
        match value(&supply.join("status")).as_deref() {
            Some("Discharging") => discharging = true,
            Some("Charging" | "Full" | "Not charging") => charging = true,
            _ => {}
        }
        let charge = value(&supply.join("capacity")).and_then(|text| text.parse::<u32>().ok());
        charges.extend(charge.filter(|charge| *charge <= PERCENT_MAX));
    }
    assert!(battery || !discharging);
    assert!(battery || !charging);
    assert!(battery || charges.is_empty());
    let source = if discharging {
        PowerSource::Battery
    } else if online || charging || !battery {
        PowerSource::Ac
    } else {
        PowerSource::Unknown
    };
    assert!(charges.len() <= ENTRIES_MAX);
    let percent = (!charges.is_empty())
        .then(|| charges.iter().sum::<u32>() / charges.len() as u32)
        .and_then(|mean| u8::try_from(mean).ok());
    assert!(percent.is_none_or(|percent| u32::from(percent) <= PERCENT_MAX));
    (source, percent)
}

/// Reads one thermal zone: its temperature against its own trip points, which the
/// computer's maker set. At or above a `critical` trip is critical, at or above a `hot`
/// trip is serious, below both is nominal. Input: the zone folder. Output: the heat, or
/// none when the zone has no readable temperature.
fn zone(folder: &Path) -> Option<Heat> {
    let now: i64 = value(&folder.join("temp"))?.parse().ok()?;
    let mut heat = Heat::Nominal;
    for index in 0..TRIPS_MAX {
        let Some(kind) = value(&folder.join(format!("trip_point_{index}_type"))) else {
            break;
        };
        let trip = value(&folder.join(format!("trip_point_{index}_temp")))
            .and_then(|text| text.parse::<i64>().ok())
            .filter(|trip| *trip > 0);
        let Some(trip) = trip.filter(|trip| now >= *trip) else {
            continue;
        };
        assert!(trip > 0 && now >= trip);
        match kind.as_str() {
            "critical" => heat = Heat::Critical,
            "hot" if heat != Heat::Critical => heat = Heat::Serious,
            _ => {}
        }
    }
    Some(heat)
}

/// Reads the heat below a `/sys/class/thermal` shaped folder: the hottest state any
/// thermal zone reports against its own trip points (DEC-167 addendum 2). Other entries
/// of the folder, cooling devices among them, never take a zone's place. Input: the
/// folder. Output: critical, serious or nominal, or unknown with no readable zone.
pub fn heat(root: &Path) -> Heat {
    let zones: Vec<Heat> = entries(root, ZONE_PREFIX)
        .iter()
        .filter_map(|path| zone(path.as_path()))
        .collect();
    assert!(zones.len() <= ENTRIES_MAX);
    if zones.contains(&Heat::Critical) {
        Heat::Critical
    } else if zones.contains(&Heat::Serious) {
        Heat::Serious
    } else if zones.is_empty() {
        Heat::Unknown
    } else {
        Heat::Nominal
    }
}

/// Reads the lid below a `/proc/acpi/button/lid` shaped folder: one folder per lid, each
/// with a `state` file that says open or closed. Input: the folder. Output: closed when
/// any lid is closed, else open when any lid is open, else none (a computer without an
/// ACPI lid, or a state that cannot be read).
pub fn lid(root: &Path) -> Option<bool> {
    let lids = entries(root, "");
    assert!(lids.len() <= ENTRIES_MAX);
    let mut open = false;
    for folder in &lids {
        let state = value(&folder.join("state"));
        let state = state
            .as_deref()
            .and_then(|text| text.strip_prefix(STATE_PREFIX))
            .map(str::trim);
        match state {
            Some("closed") => return Some(true),
            Some("open") => open = true,
            _ => {}
        }
    }
    open.then_some(false)
}

/// Reads the power source, charge, heat and lid of a Linux computer from files below
/// `root`, with no bus connection. The platform can hold the closed lid where logind
/// left its `run/systemd/seats` folder. Input: the file system root, or a stand shaped
/// like it. Output: the reading, unknown where the files say nothing.
pub fn read_under(root: &Path) -> PowerReading {
    assert!(root.is_absolute());
    let (source, battery_pct) = supplies(&root.join(SUPPLIES));
    let unknown = PowerReading::unknown();
    assert_ne!(unknown.lid_support, LidSupport::Held);
    PowerReading {
        source,
        battery_pct,
        heat: heat(&root.join(THERMAL)),
        lid_closed: lid(&root.join(LIDS)),
        external_display: None,
        lid_support: if root.join(SEATS).is_dir() {
            LidSupport::Held
        } else {
            unknown.lid_support
        },
    }
}

/// Reads this computer's power source, charge, heat and lid, with no bus connection.
/// Input: none, the files sit below the real file system root. Output: the reading,
/// unknown where the files say nothing.
#[cfg(target_os = "linux")]
pub(crate) fn read() -> PowerReading {
    read_under(Path::new("/"))
}
