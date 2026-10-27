use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The file in a project's IDE folder that records its two loopback ports.
pub const FILE: &str = "port";

const FILE_BYTES_MAX: u64 = 32;

/// The two loopback ports one project's IDE uses: the server's own and the engine proxy's
/// in front of it, which the desktop opens. The browser keeps the IDE's layout and open
/// editors per origin, and the origin holds the proxy port, so each project keeps its ports
/// across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub server: u16,
    pub proxy: u16,
}

/// Reads the ports a project recorded. Input: the project's IDE folder. Output: the ports
/// when the file is a small plain file holding two distinct nonzero ports, else none.
pub fn recorded(folder: &Path) -> Option<Ports> {
    let path = folder.join(FILE);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > FILE_BYTES_MAX {
        return None;
    }
    let text = fs::read_to_string(&path).ok()?;
    let (server, proxy) = text.strip_suffix('\n')?.split_once(' ')?;
    let ports = Ports {
        server: server.parse::<u16>().ok().filter(|port| *port != 0)?,
        proxy: proxy.parse::<u16>().ok().filter(|port| *port != 0)?,
    };
    (ports.server != ports.proxy).then_some(ports)
}

/// Records a project's ports, replacing the file whole so a reader never sees half of it.
/// Inputs: the project's private IDE folder and two distinct nonzero ports. Output: done.
pub fn record(folder: &Path, ports: Ports) -> io::Result<()> {
    assert!(ports.server != 0 && ports.proxy != 0 && ports.server != ports.proxy);
    let staged = folder.join(format!("{FILE}.new"));
    if let Err(error) = fs::remove_file(&staged)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)?;
    file.write_all(format!("{} {}\n", ports.server, ports.proxy).as_bytes())?;
    file.sync_all()?;
    fs::rename(&staged, folder.join(FILE))?;
    assert_eq!(recorded(folder), Some(ports));
    Ok(())
}

/// Tells whether a loopback port is free now: a listener binds to it on 127.0.0.1 and is
/// closed again. Input: the port. Output: the answer.
pub fn free(port: u16) -> bool {
    port != 0 && crate::spawn_gate::bind_tcp_std(("127.0.0.1", port)).is_ok()
}

/// Chooses a loopback port that is free now and is not `taken`. Input: a port already in
/// use by this project. Output: the port the system handed out.
pub fn pick(taken: u16) -> io::Result<u16> {
    for _ in 0..8 {
        let port = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0))?
            .local_addr()?
            .port();
        if port != 0 && port != taken {
            return Ok(port);
        }
    }
    Err(io::Error::other("ide_port_unavailable"))
}
