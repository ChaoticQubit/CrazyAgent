use super::*;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, DuplicateTokenEx, PSID, RevertToSelf,
    SID_AND_ATTRIBUTES, SecurityImpersonation, TOKEN_ALL_ACCESS, TOKEN_DUPLICATE, TOKEN_QUERY,
    TokenImpersonation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, SetThreadToken};

/// The RESTRICTED SID, the usual restricting SID of a restricted token.
const RESTRICTED: &str = "S-1-5-12";
/// The impersonation level a client allows its server, SECURITY_IDENTIFICATION.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

/// An engine in a fresh temp folder and the endpoint path in its private root. Output: the
/// temp folder, kept alive by the caller, the engine, its settings and the endpoint path.
async fn engine_at_a_private_root() -> (
    tempfile::TempDir,
    Arc<Engine>,
    EffectiveConfig,
    std::path::PathBuf,
) {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("g");
    std::fs::write(&global, "").unwrap();
    let config = crate::config::resolve_config(&global, &temp.path().join("p")).unwrap();
    let engine = Engine::open(&temp.path().join("db"), config.clone())
        .await
        .unwrap();
    let root = temp.path().join("run");
    drop(crazyagent_win_sandbox::private::dir(&root).unwrap());
    (temp, Arc::new(engine), config, root.join("engine.sock"))
}

/// One project create as a desktop client sends it, for a repository in `folder`. Inputs:
/// the request id and the folder. Output: the request.
fn create_request(id: &str, folder: &Path) -> WireRequest {
    WireRequest {
        protocol_version: crate::protocol::PROTOCOL_VERSION,
        client_id: ClientId::new("desktop").unwrap(),
        request_id: crate::protocol::RequestId::new(id).unwrap(),
        expected_revision: crate::protocol::Cursor::new(0),
        command: crate::protocol::Command::CreateProject {
            project_id: crate::protocol::ProjectId::new(id).unwrap(),
            spec: crate::protocol::ProjectSpec {
                name: "Inventory".into(),
                repositories: vec![folder.join("inventory").display().to_string()],
            },
        },
    }
}

/// A restricted impersonation copy of this process's token with RESTRICTED as its only
/// restricting SID, which no engine pipe list grants. Output: the token.
fn restricted_token() -> OwnedHandle {
    let wide: Vec<u16> = RESTRICTED
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut restricting: PSID = null_mut();
    assert_ne!(
        unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut restricting) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    let mut process = null_mut();
    assert_ne!(
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut process,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let entry = SID_AND_ATTRIBUTES {
        Sid: restricting,
        Attributes: 0,
    };
    let mut restricted = null_mut();
    let made = unsafe {
        CreateRestrictedToken(
            process.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            0,
            null(),
            0,
            null(),
            1,
            &entry,
            &mut restricted,
        )
    };
    let failure = io::Error::last_os_error();
    unsafe { LocalFree(restricting) };
    assert_ne!(made, 0, "{failure}");
    let restricted = unsafe { OwnedHandle::from_raw_handle(restricted) };
    let mut impersonation = null_mut();
    assert_ne!(
        unsafe {
            DuplicateTokenEx(
                restricted.as_raw_handle(),
                TOKEN_ALL_ACCESS,
                null(),
                SecurityImpersonation,
                TokenImpersonation,
                &mut impersonation,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    unsafe { OwnedHandle::from_raw_handle(impersonation) }
}

/// Runs `work` on this thread while it impersonates `token`, then reverts; a failed revert
/// aborts the process, so no later code runs as the lowered principal. Inputs: the token
/// and the work. Output: what the work returned.
fn as_principal<T>(token: &OwnedHandle, work: impl FnOnce() -> T) -> T {
    assert_ne!(
        unsafe { SetThreadToken(null(), token.as_raw_handle()) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    let result = work();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    result
}

/// WIRE-2: an ordinary request reaches the engine over its pipe and its reply carries the
/// same request id.
#[tokio::test(flavor = "multi_thread")]
async fn an_ordinary_request_round_trips_over_the_engine_pipe() {
    let (temp, engine, config, socket) = engine_at_a_private_root().await;
    let server = Server::bind(&socket, engine, &config).unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let client = Client::new(socket.clone(), Duration::from_secs(10)).unwrap();
    let reply = client
        .request(create_request("round-trip", temp.path()))
        .await
        .unwrap();
    assert_eq!(reply.request_id.as_str(), "round-trip");
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
}

/// WIRE-2: a root that is not private, as a plain folder made in the temp folder is, is
/// refused at bind, and no pipes file is written into it.
#[tokio::test(flavor = "multi_thread")]
async fn a_bind_in_a_root_that_is_not_private_is_refused() {
    let (temp, engine, config, _) = engine_at_a_private_root().await;
    let plain = temp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let refused = Server::bind(&plain.join("engine.sock"), engine, &config);
    assert!(refused.is_err());
    assert!(std::fs::read_dir(&plain).unwrap().next().is_none());
}

/// WIRE-2: a second engine on a root another engine serves is refused with
/// `engine_pipe_taken`, and the first keeps serving.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_engine_on_a_served_root_is_refused_with_engine_pipe_taken() {
    let (temp, engine, config, socket) = engine_at_a_private_root().await;
    let first = Server::bind(&socket, engine.clone(), &config).unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(first.run(stopped));
    let refused = Server::bind(&socket, engine, &config).err().unwrap();
    assert_eq!(refused.0, "engine_pipe_taken");
    let client = Client::new(socket.clone(), Duration::from_secs(10)).unwrap();
    let reply = client
        .request(create_request("still-served", temp.path()))
        .await
        .unwrap();
    assert_eq!(reply.request_id.as_str(), "still-served");
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
}

/// R1: a rogue instance under a pipes file that pins another start gets no byte from a
/// request; the client refuses it with `peer_owner_mismatch` before it writes.
#[tokio::test(flavor = "multi_thread")]
async fn a_rogue_instance_with_another_pin_gets_no_byte_from_a_request() {
    let (temp, _engine, _config, socket) = engine_at_a_private_root().await;
    let rogue = crate::ipc::serve_unpinned(&socket).unwrap();
    let reading = tokio::spawn(async move {
        rogue.connect().await.unwrap();
        let mut rogue = rogue;
        let mut bytes = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), rogue.read_to_end(&mut bytes)).await;
        bytes
    });
    let client = Client::new(socket, Duration::from_secs(10)).unwrap();
    let refused = client
        .request(create_request("to-rogue", temp.path()))
        .await
        .unwrap_err();
    assert_eq!(refused.0, "peer_owner_mismatch");
    assert!(reading.await.unwrap().is_empty());
}

/// R4: a client on a restricted token is refused before any frame is parsed: it gets no
/// reply to a request the same user's ordinary client gets one for.
#[tokio::test(flavor = "multi_thread")]
async fn a_restricted_client_is_refused_before_any_frame_is_parsed() {
    let (temp, engine, config, socket) = engine_at_a_private_root().await;
    let server = Server::bind(&socket, engine, &config).unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let name = crate::ipc::pipe_name_at(&socket).unwrap();
    let frame = encode_frame(&create_request("restricted", temp.path())).unwrap();
    let token = restricted_token();
    let answered = tokio::task::spawn_blocking(move || {
        as_principal(&token, || {
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&name);
            let Ok(mut pipe) = opened else {
                return Vec::new();
            };
            use std::io::{Read, Write};
            let _ = pipe.write_all(&frame);
            let mut reply = Vec::new();
            let _ = pipe.read_to_end(&mut reply);
            reply
        })
    })
    .await
    .unwrap();
    assert!(answered.is_empty(), "{} reply bytes", answered.len());
    let client = Client::new(socket.clone(), Duration::from_secs(10)).unwrap();
    let reply = client
        .request(create_request("restricted", temp.path()))
        .await
        .unwrap();
    assert_eq!(reply.request_id.as_str(), "restricted");
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
}

/// WIRE-2: once `run` stops, the pipes file is gone from the root and the endpoint no
/// longer answers.
#[tokio::test(flavor = "multi_thread")]
async fn after_run_stops_the_pipes_file_is_gone_and_the_endpoint_does_not_answer() {
    let (_temp, engine, config, socket) = engine_at_a_private_root().await;
    let server = Server::bind(&socket, engine, &config).unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    assert!(crate::service::answering(&socket));
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    assert!(!socket.parent().unwrap().join("pipes").exists());
    assert!(!crate::service::answering(&socket));
}
