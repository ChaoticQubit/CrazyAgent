use std::path::PathBuf;

use super::*;

/// Names a folder of the sdk tree. Input: its path below `crates/sdk`. Output: the folder.
fn sdk(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path)
}

/// The binding generated from the checked-in `.cap` files. Output: its one file.
fn binding_from_caps() -> Vec<(String, String)> {
    let loaded = load(&sdk("caps")).expect("the caps load");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    binding(&borrowed).expect("the binding generates")
}

/// The binding generated from in-test sources. Input: stems with sources. Output: the
/// text with every run of whitespace removed, so rows read past rustfmt's layout.
fn flat_binding(sources: &[(&str, &str)]) -> String {
    let parsed: Vec<(&str, File)> = sources
        .iter()
        .map(|(stem, source)| {
            (
                *stem,
                crazyagent_cap::parse_as(stem, source).expect("parses"),
            )
        })
        .collect();
    let files = binding(&parsed).expect("the binding generates");
    assert_eq!(files.len(), 1);
    files[0].1.split_whitespace().collect()
}

#[test]
fn host_mod_rs_equals_capgen_output() {
    let files = binding_from_caps();
    assert_eq!(
        files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        ["mod.rs"]
    );
    assert_eq!(
        drift(&sdk("host/src/generated"), &files),
        Vec::<String>::new()
    );
}

/// The stems the prod binding serves through a plugin, not through a kernel provider.
const SERVED: [&str; 1] = ["projects"];

#[test]
fn the_host_binding_is_generated_for_every_capability() {
    let loaded = load(&sdk("caps")).expect("the caps load");
    let files = binding_from_caps();
    let flat: String = files[0].1.split_whitespace().collect();
    let mut stems: Vec<&str> = loaded.iter().map(|(stem, _)| stem.as_str()).collect();
    stems.sort_unstable();
    assert!(stems.len() >= 6);
    let joined = stems.join(",");
    let used = format!("usebus_gateway::Caller;usesdk::Refusal;usesdk::generated::{{{joined}}};");
    let wrapped =
        format!("usebus_gateway::Caller;usesdk::Refusal;usesdk::generated::{{{joined},}};");
    assert!(
        flat.starts_with(&used) || flat.starts_with(&wrapped),
        "{used} or {wrapped}"
    );
    assert!(stems.contains(&"projects"));
    let provided: Vec<&str> = stems
        .iter()
        .copied()
        .filter(|stem| !SERVED.contains(stem))
        .collect();
    let fields: String = provided
        .iter()
        .map(|stem| format!("pub{stem}:&'pdyn{stem}::Provide,"))
        .collect();
    let providers = format!("pubstructProviders<'p>{{{fields}}}");
    assert!(flat.contains(&providers), "{providers}");
    assert!(!flat.contains("pubprojects:&'pdynprojects::Provide"));
    assert!(flat.contains(
        "pubstructBound<'g,'p>{caller:Caller<'g>,providers:&'pProviders<'p>,served:&'pServed<'p>,}"
    ));
    let mut count = 0;
    let mut served = 0;
    for (stem, file) in &loaded {
        if SERVED.contains(&stem.as_str()) {
            served += file.capabilities.len();
            continue;
        }
        for capability in &file.capabilities {
            let function = capability.name[stem.len() + 1..].replace('.', "_");
            let constant = function.to_ascii_uppercase();
            let (signature, provided) = if capability.inputs.is_empty() {
                (
                    format!("fn{stem}_{function}(&self)->"),
                    format!("self.providers.{stem}.{function}(self.caller.plugin())"),
                )
            } else {
                (
                    format!("fn{stem}_{function}(&self,"),
                    format!("self.providers.{stem}.{function}(self.caller.plugin(),"),
                )
            };
            for part in [
                signature,
                format!("self.caller.call(&{stem}::{constant},"),
                format!("{stem}::check_{function}_input"),
                provided,
                format!("{stem}::check_{function}_output,)"),
            ] {
                assert!(flat.contains(&part), "{part}");
            }
            count += 1;
        }
    }
    assert!(count > 0);
    assert!(
        loaded
            .iter()
            .flat_map(|(_, file)| &file.capabilities)
            .any(|capability| capability.inputs.is_empty()),
        "no capability without inputs"
    );
    assert_eq!(flat.matches("self.caller.call(").count(), count);
    assert_eq!(served, 1);
    assert_eq!(flat.matches("self.caller.serve(").count(), served);
    assert!(flat.contains(
        "fnsettings_save_quit(&self,settings:sdk::generated::settings::QuitSettings,)->Result<(),Refusal>{self.caller.call(&settings::SAVE_QUIT,settings,settings::check_save_quit_input,|settings|{self.providers.settings.save_quit(self.caller.plugin(),settings)},settings::check_save_quit_output,)}"
    ));
    assert!(flat.contains(
        "fnsecrets_find(&self,name:alloc::string::String,)->Result<core::option::Option<sdk::generated::secrets::Secret>,Refusal>{self.caller.call(&secrets::FIND,name,|name|secrets::check_find_input(name),|name|self.providers.secrets.find(self.caller.plugin(),name),secrets::check_find_output,)}"
    ));
}

#[test]
fn a_call_taking_a_foreign_handle_binds_its_inputs_as_one_tuple() {
    let flat = flat_binding(&[
        (
            "net",
            "type Ring { keys: list<secrets.Secret> }\ncapability net.send(keys: list<secrets.Secret>, ring: Ring, s: secrets.Secret) -> unit tier: kernel needs: net.send",
        ),
        ("secrets", "handle Secret"),
    ]);
    assert!(
        flat.starts_with("usebus_gateway::Caller;usesdk::Refusal;usesdk::generated::net;"),
        "{flat}"
    );
    assert!(
        flat.contains("pubstructProviders<'p>{pubnet:&'pdynnet::Provide,}"),
        "{flat}"
    );
    assert!(flat.contains(
        "fnnet_send(&self,keys:alloc::vec::Vec<sdk::generated::secrets::Secret>,ring:sdk::generated::net::Ring,s:sdk::generated::secrets::Secret,)->Result<(),Refusal>{self.caller.call(&net::SEND,(keys,ring,s),|(keys,ring,s)|net::check_send_input(keys,ring,s),|(keys,ring,s)|self.providers.net.send(self.caller.plugin(),keys,ring,s),net::check_send_output,)}"
    ), "{flat}");
}

#[test]
fn a_call_without_inputs_binds_the_unit_value() {
    let flat = flat_binding(&[(
        "clock",
        "capability clock.now() -> u64 tier: kernel needs: clock.read",
    )]);
    assert!(flat.contains(
        "fnclock_now(&self)->Result<u64,Refusal>{self.caller.call(&clock::NOW,(),|_|clock::check_now_input(),|_|self.providers.clock.now(self.caller.plugin()),clock::check_now_output,)}"
    ), "{flat}");
}

#[test]
fn a_binding_with_no_capability_is_refused() {
    let parsed = vec![(
        "secrets",
        crazyagent_cap::parse_as("secrets", "handle Secret").expect("parses"),
    )];
    assert_eq!(binding(&parsed).unwrap_err().code, "capgen_no_capability");
}

/// The fixture binding generated from `capgen/fixture_served`. Output: its one file.
fn served_binding() -> Vec<(String, String)> {
    let loaded = load(&sdk("capgen/fixture_served")).expect("the fixture loads");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    test_binding(&borrowed).expect("the fixture binding generates")
}

#[test]
fn the_fixture_binding_equals_capgen_output() {
    let files = served_binding();
    assert_eq!(
        files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        ["mod.rs"]
    );
    assert_eq!(
        drift(&sdk("host/src/fixture_served"), &files),
        Vec::<String>::new()
    );
}

#[test]
fn the_binding_writes_served_unserved_and_bound_serving_only_with_a_served_stem() {
    let kernel_only = flat_binding(&[(
        "clock",
        "capability clock.now() -> u64 tier: kernel needs: clock.read",
    )]);
    for absent in ["Served", "Unserved", "fnserving(", "served:", "::Serve"] {
        assert!(!kernel_only.contains(absent), "{absent}");
    }
    let prod: String = binding_from_caps()[0].1.split_whitespace().collect();
    assert!(
        prod.contains("pubstructServed<'p>{pubprojects:&'pdynprojects::Serve,}"),
        "{prod}"
    );
    assert!(prod.contains("pubconstNONE:Served<'static>=Served{projects:&Unserved,};"));
    assert_eq!(
        prod.matches("self.caller.serve(&projects::LIST,").count(),
        1
    );
    assert_eq!(prod.matches("self.caller.serve(").count(), 1);
    let flat: String = served_binding()[0].1.split_whitespace().collect();
    assert!(
        flat.starts_with(
            "usebus_gateway::Caller;usesdk::Refusal;usesdk::fixture_served::{clock,echo,tick};"
        ),
        "{flat}"
    );
    for part in [
        "pubstructProviders<'p>{pubclock:&'pdynclock::Provide,}",
        "pubstructServed<'p>{pubecho:&'pdynecho::Serve,pubtick:&'pdyntick::Serve,}",
        "pubstructUnserved;",
        "implecho::ServeforUnserved{fnping(&self,_host:&dynsdk::fixture_served::host::Host,_n:u32)->Result<u32,Refusal>{Err(Refusal::Failed{code:\"capability_unserved\".into(),})}}",
        "impltick::ServeforUnserved{fncount(&self,_host:&dynsdk::fixture_served::host::Host)->Result<u32,Refusal>{Err(Refusal::Failed{code:\"capability_unserved\".into(),})}}",
        "implServed<'static>{",
        "pubconstNONE:Served<'static>=Served{echo:&Unserved,tick:&Unserved,};",
        "pubstructBound<'g,'p>{caller:Caller<'g>,providers:&'pProviders<'p>,served:&'pServed<'p>,}",
        "pubfnnew(caller:Caller<'g>,providers:&'pProviders<'p>)->Self{Self{caller,providers,served:&Served::NONE,}}",
        "pubfnserving(caller:Caller<'g>,providers:&'pProviders<'p>,served:&'pServed<'p>,)->Self{Self{caller,providers,served,}}",
        "implsdk::fixture_served::host::HostforBound<'_,'_>{",
    ] {
        assert!(flat.contains(part), "{part}\n{flat}");
    }
}

#[test]
fn a_served_method_calls_caller_serve_and_a_fresh_bound() {
    let flat: String = served_binding()[0].1.split_whitespace().collect();
    for part in [
        "fnecho_ping(&self,n:u32)->Result<u32,Refusal>{self.caller.serve(&echo::PING,n,echo::check_ping_input,|provider,n|{lethost=Bound{caller:provider,providers:self.providers,served:self.served,};self.served.echo.ping(&host,n)},echo::check_ping_output,)}",
        "fntick_count(&self)->Result<u32,Refusal>{self.caller.serve(&tick::COUNT,(),|_|tick::check_count_input(),|provider,_|{lethost=Bound{caller:provider,providers:self.providers,served:self.served,};self.served.tick.count(&host)},tick::check_count_output,)}",
        "fnclock_now(&self)->Result<u64,Refusal>{self.caller.call(&clock::NOW,(),|_|clock::check_now_input(),|_|self.providers.clock.now(self.caller.plugin()),clock::check_now_output,)}",
    ] {
        assert!(flat.contains(part), "{part}\n{flat}");
    }
    assert_eq!(flat.matches("self.caller.serve(").count(), 2);
    assert_eq!(flat.matches("self.caller.call(").count(), 1);
    assert!(!flat.contains("self.caller.plugin(),"), "{flat}");
}

#[test]
fn a_binding_with_no_kernel_stem_is_refused() {
    let parsed = vec![(
        "echo",
        crazyagent_cap::parse_as(
            "echo",
            "capability echo.ping() -> unit tier: t1 needs: echo.call",
        )
        .expect("parses"),
    )];
    match test_binding(&parsed) {
        Ok(files) => panic!("generated, expected a refusal: {files:?}"),
        Err(error) => assert_eq!(error.code, "capgen_no_provided_stem"),
    }
}
