use super::power::PowerLine;
use super::power_saver::{PowerSaver, Saver, SaverError};
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{OwnedValue, Value};

const BUS_WAIT: Duration = Duration::from_secs(2);
const HOLDS_MAX: usize = 64;
const PROFILE: &str = "power-saver";
const APP_ID: &str = "dev.crazyagent";
const REASON: &str = "CrazyAgent keeps this computer awake on battery in power saver";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The daemon's names on the system bus, newest first: the service, the object and the
/// interface. Releases before 0.20 answer only on the second (QUESTION 39g).
const NAMES: [(&str, &str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    ),
    (
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "net.hadess.PowerProfiles",
    ),
];

/// power-profiles-daemon on the system bus (QUESTION 39g): the connection, kept while a
/// hold lives since the daemon drops the holds of a caller that left the bus, and which of
/// `NAMES` answered on it.
#[derive(Default)]
struct Daemon {
    bus: Option<zbus::blocking::Connection>,
    names: Option<usize>,
}

/// Builds the power line around power-profiles-daemon. Output: the line, which makes no
/// bus call until the sensor thread hands it its first ask.
pub(super) fn power_line() -> PowerLine {
    let mut line = PowerSaver::new(Box::new(Daemon::default()));
    Box::new(move |wanted, now_ms| {
        assert!(now_ms > 0);
        line.step(wanted)
    })
}

/// Sorts a bus error into why the daemon did not answer as asked. Input: the error.
/// Output: not running when no one owns the name, refused when the system denied the
/// call, else failed with the error's text.
fn sorted(problem: zbus::Error) -> SaverError {
    let zbus::Error::MethodError(name, _, _) = &problem else {
        return SaverError::Failed(problem.to_string());
    };
    let name: &str = name.as_ref();
    match name {
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner" => SaverError::NotRunning,
        "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.AuthFailed"
        | "org.freedesktop.PolicyKit1.Error.NotAuthorized" => SaverError::Refused,
        _ => SaverError::Failed(problem.to_string()),
    }
}

/// One text field of a hold the daemon lists, as an `a{sv}` entry. Inputs: the hold and
/// the key. Output: the text, or none when the key is missing or not text.
fn text<'h>(hold: &'h HashMap<String, OwnedValue>, key: &str) -> Option<&'h str> {
    let value: &Value<'static> = hold.get(key)?;
    let value = match value {
        Value::Value(inner) => &**inner,
        plain => plain,
    };
    match value {
        Value::Str(text) => Some(text.as_str()),
        _ => None,
    }
}

impl Daemon {
    /// The system bus connection, made once; its calls wait at most `BUS_WAIT`. Output:
    /// the connection, or why it could not be made.
    fn bus(&mut self) -> Result<&zbus::blocking::Connection, SaverError> {
        let bus = match self.bus.take() {
            Some(bus) => bus,
            None => zbus::blocking::connection::Builder::system()
                .and_then(|builder| builder.method_timeout(BUS_WAIT).build())
                .map_err(|problem| SaverError::Failed(format!("system bus: {problem}")))?,
        };
        Ok(self.bus.insert(bus))
    }

    /// Calls one method of the daemon on the names that answered. A daemon that left the
    /// bus is looked for again on the next call. Inputs: which of `NAMES`, the interface
    /// or none for the daemon's own, the method and its arguments. Output: the reply, or
    /// why not.
    fn call<B>(
        &mut self,
        names: usize,
        interface: Option<&str>,
        method: &str,
        body: &B,
    ) -> Result<zbus::Message, SaverError>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        assert!(names < NAMES.len());
        let (service, object, own) = NAMES[names];
        let called = self.bus()?.call_method(
            Some(service),
            object,
            Some(interface.unwrap_or(own)),
            method,
            body,
        );
        let reply = called.map_err(sorted);
        if reply.as_ref().is_err_and(|problem| *problem == SaverError::NotRunning) {
            self.names = None;
        }
        reply
    }

    /// Reads one property of the daemon. Inputs: which of `NAMES` and the property.
    /// Output: its value, or why not.
    fn property(&mut self, names: usize, property: &str) -> Result<OwnedValue, SaverError> {
        let own = NAMES[names].2;
        let reply = self.call(names, Some(PROPERTIES), "Get", &(own, property))?;
        let value = reply.body().deserialize::<OwnedValue>();
        value.map_err(|problem| SaverError::Failed(format!("{property}: {problem}")))
    }

    /// Finds which of `NAMES` the daemon answers on, once until it leaves the bus.
    /// Output: the index, or why no name answered, the first failure that is not a
    /// missing daemon first.
    fn names(&mut self) -> Result<usize, SaverError> {
        if let Some(names) = self.names {
            return Ok(names);
        }
        let mut why = SaverError::NotRunning;
        for names in 0..NAMES.len() {
            match self.property(names, "ActiveProfile") {
                Ok(_) => {
                    self.names = Some(names);
                    return Ok(names);
                }
                Err(SaverError::NotRunning) => {}
                Err(problem) if why == SaverError::NotRunning => why = problem,
                Err(_) => {}
            }
        }
        assert!(self.names.is_none());
        Err(why)
    }
}

impl Saver for Daemon {
    fn present(&mut self) -> Result<(), SaverError> {
        self.names().map(|_| ())
    }

    fn hold(&mut self) -> Result<u32, SaverError> {
        let names = self.names()?;
        let reply = self.call(names, None, "HoldProfile", &(PROFILE, REASON, APP_ID))?;
        let cookie = reply.body().deserialize::<u32>();
        cookie.map_err(|problem| SaverError::Failed(format!("HoldProfile: {problem}")))
    }

    fn holds_ours(&mut self) -> Result<bool, SaverError> {
        let names = self.names()?;
        let value = self.property(names, "ActiveProfileHolds")?;
        let holds = Vec::<HashMap<String, OwnedValue>>::try_from(value);
        let holds = holds.map_err(|problem| SaverError::Failed(format!("holds: {problem}")))?;
        let ours = holds.iter().take(HOLDS_MAX).any(|hold| {
            text(hold, "ApplicationId") == Some(APP_ID) && text(hold, "Profile") == Some(PROFILE)
        });
        Ok(ours)
    }

    /// Ends the hold; when the daemon cannot be told, the bus connection is closed, which
    /// makes the daemon drop every hold this engine has.
    fn release(&mut self, cookie: u32) {
        let released = match self.names {
            Some(names) => self.call(names, None, "ReleaseProfile", &cookie).map(|_| ()),
            None => Err(SaverError::NotRunning),
        };
        if let Err(problem) = released {
            eprintln!("keep awake: power saver let go by closing the bus: {problem:?}");
            self.bus = None;
            self.names = None;
        }
        assert!(self.bus.is_some() || self.names.is_none());
    }
}
