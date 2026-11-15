use super::state::{Heat, PowerSource};
use std::io::Read;
use std::path::{Path, PathBuf};

const ENTRIES_MAX: usize = 64;
const TRIPS_MAX: usize = 16;
const VALUE_BYTES_MAX: u64 = 64;
const PERCENT_MAX: u32 = 100;

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

/// Lists the entries of one sysfs folder by name, at most `ENTRIES_MAX`. Input: the
/// folder. Output: the entry paths in name order, empty when the folder cannot be read.
fn entries(folder: &Path) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = listing
        .take(ENTRIES_MAX)
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    found.sort();
    assert!(found.len() <= ENTRIES_MAX);
    found
}

/// Reads the power supplies below a `/sys/class/power_supply` shaped folder. A system
/// battery (not one of a mouse or keyboard) that discharges means battery, whatever else
/// is attached. Otherwise a mains or USB supply that is online, or a battery that charges
/// or is full, means AC. The charge is the mean of the system batteries. Input: the
/// folder. Output: the source and the percent, unknown and none where nothing says.
pub fn supplies(root: &Path) -> (PowerSource, Option<u8>) {
    let mut online = false;
    let mut discharging = false;
    let mut charging = false;
    let mut charges: Vec<u32> = Vec::new();
    for supply in entries(root) {
        let kind = value(&supply.join("type")).unwrap_or_default();
        if kind != "Battery" {
            online |= value(&supply.join("online")).as_deref() == Some("1");
            continue;
        }
        if value(&supply.join("scope")).as_deref() == Some("Device") {
            continue;
        }
        match value(&supply.join("status")).as_deref() {
            Some("Discharging") => discharging = true,
            Some("Charging" | "Full" | "Not charging") => charging = true,
            _ => {}
        }
        let charge = value(&supply.join("capacity")).and_then(|text| text.parse::<u32>().ok());
        charges.extend(charge.filter(|charge| *charge <= PERCENT_MAX));
    }
    let source = match (discharging, online || charging) {
        (true, _) => PowerSource::Battery,
        (false, true) => PowerSource::Ac,
        (false, false) => PowerSource::Unknown,
    };
    assert!(charges.len() <= ENTRIES_MAX);
    let percent = (!charges.is_empty())
        .then(|| charges.iter().sum::<u32>() / charges.len() as u32)
        .and_then(|mean| u8::try_from(mean).ok());
    assert!(percent.is_none_or(|percent| u32::from(percent) <= PERCENT_MAX));
    (source, percent)
}

/// Settles the power source where the supply files say nothing (a desktop or a virtual
/// machine has none): logind's own answer decides. Inputs: the source from the supply
/// files and logind's `OnExternalPower`, none when logind did not answer. Output: the
/// source; one the supply files gave is kept.
pub fn source(sysfs: PowerSource, external: Option<bool>) -> PowerSource {
    match (sysfs, external) {
        (PowerSource::Unknown, Some(true)) => PowerSource::Ac,
        (PowerSource::Unknown, Some(false)) => PowerSource::Battery,
        (known, _) => known,
    }
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
/// thermal zone reports against its own trip points (DEC-167 addendum 2). Input: the
/// folder. Output: critical, serious or nominal, or unknown with no readable zone.
pub fn heat(root: &Path) -> Heat {
    let zones: Vec<Heat> = entries(root)
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("thermal_zone"))
        })
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

#[cfg(target_os = "linux")]
mod live {
    use super::super::power::PowerReading;
    use super::super::state::{LidSupport, PowerSource};
    use std::path::Path;
    use std::time::Duration;

    const BUS_WAIT: Duration = Duration::from_secs(2);
    const SERVICE: &str = "org.freedesktop.login1";
    const OBJECT: &str = "/org/freedesktop/login1";
    const MANAGER: &str = "org.freedesktop.login1.Manager";
    const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
    const SUPPLIES: &str = "/sys/class/power_supply";
    const THERMAL: &str = "/sys/class/thermal";

    /// The reader's connection to logind on the system bus: opened at the first use, kept
    /// between readings, and opened again after a call failed.
    #[derive(Debug, Default)]
    pub(crate) struct Logind {
        bus: Option<zbus::blocking::Connection>,
    }

    impl Logind {
        /// Reads one true or false property of the logind manager, waiting at most
        /// `BUS_WAIT` for the answer. Input: the property name. Output: the value, or
        /// none when the bus or logind does not answer; the connection is then dropped.
        fn flag(&mut self, name: &str) -> Option<bool> {
            assert!(!name.is_empty());
            if self.bus.is_none() {
                self.bus = zbus::blocking::connection::Builder::system()
                    .and_then(|builder| builder.method_timeout(BUS_WAIT).build())
                    .ok();
            }
            let reply = self.bus.as_ref()?.call_method(
                Some(SERVICE),
                OBJECT,
                Some(PROPERTIES),
                "Get",
                &(MANAGER, name),
            );
            let Ok(reply) = reply else {
                self.bus = None;
                return None;
            };
            let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
            bool::try_from(value).ok()
        }
    }

    /// Reads this computer's power source, charge and heat from sysfs and the lid from
    /// logind, which also settles the source where sysfs says nothing. The platform can
    /// hold the closed lid whenever logind answers. Input: the logind connection. Output:
    /// the reading, unknown where a source gave nothing.
    pub(crate) fn read(logind: &mut Logind) -> PowerReading {
        let (sysfs, battery_pct) = super::supplies(Path::new(SUPPLIES));
        let lid_closed = logind.flag("LidClosed");
        let external = match sysfs {
            PowerSource::Unknown => logind.flag("OnExternalPower"),
            PowerSource::Ac | PowerSource::Battery => None,
        };
        let unknown = PowerReading::unknown();
        assert_ne!(unknown.lid_support, LidSupport::Held);
        PowerReading {
            source: super::source(sysfs, external),
            battery_pct,
            heat: super::heat(Path::new(THERMAL)),
            lid_closed,
            lid_support: match lid_closed {
                Some(_) => LidSupport::Held,
                None => unknown.lid_support,
            },
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) use live::{Logind, read};
