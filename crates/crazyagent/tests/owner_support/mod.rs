use crazyagent::config::resolve_config;
use crazyagent::engine::Engine;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A running engine owner on a test store and the runtime it runs on, the engine first so
/// it drops before its runtime.
pub type Owner = (Engine, tokio::runtime::Runtime);

/// Opens the engine on `<engine>/engine.db`, the store `landing::open` reads, and starts its
/// owner, so the landing writes a row makes run under a running engine host as in the app
/// (v2.5 L9, QUESTION-1638 1 A). Inputs: a private folder for the config and the engine
/// folder, both existing. Output: the owner, kept for the row's life.
pub fn owner_on(private: &Path, engine: &Path) -> Owner {
    assert!(private.is_dir(), "the config folder's parent exists");
    assert!(engine.is_dir(), "the engine folder exists");
    std::fs::set_permissions(engine, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let config = resolve_config(&private.join("g"), &private.join("p")).unwrap();
    let store = engine.join("engine.db");
    let owner = runtime.block_on(async {
        let owner = Engine::open(&store, config).await.unwrap();
        owner.start_owner_runtime().await.unwrap();
        owner
    });
    assert!(
        store.is_file(),
        "the owner runs on the store the rows write"
    );
    (owner, runtime)
}
