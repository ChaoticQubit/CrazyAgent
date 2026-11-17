use super::*;
use crate::credentials::Keychain;
use crate::credentials::test_keychain::TestKeychain;

pub(super) struct Scratch {
    pub(super) root: tempfile::TempDir,
    pub(super) namespace: String,
    pub(super) store: Arc<Keychain>,
    _keychain: TestKeychain,
}

impl Scratch {
    pub(super) fn new() -> Self {
        let mut bytes = [0_u8; 6];
        getrandom::fill(&mut bytes).unwrap();
        let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let namespace = format!("dev.crazyagent.test-{suffix}");
        let mut keychain = TestKeychain::temporary();
        keychain.serve(&namespace);
        Self {
            root: tempfile::tempdir().unwrap(),
            store: Arc::new(Keychain::new(&namespace).unwrap()),
            namespace,
            _keychain: keychain,
        }
    }

    /// A scratch whose root's engine.db is seeded for logged landing writes with the test
    /// engine host running, since publish prompt answers are logged (v2.5 L9). Output: the
    /// scratch.
    pub(super) fn logged() -> Self {
        let scratch = Self::new();
        let path = scratch
            .root
            .path()
            .canonicalize()
            .unwrap()
            .join("engine.db");
        crate::landing_log::seed(&rusqlite::Connection::open(path).unwrap());
        scratch
    }

    pub(super) fn account(&self, server: &Server) -> GitHub {
        GitHub::new(
            &self.root.path().canonicalize().unwrap(),
            self.namespace.clone(),
            Arc::clone(&self.store) as Arc<dyn Store>,
            Endpoints::loopback(server.port),
        )
    }

    pub(super) fn state(&self) -> Option<String> {
        std::fs::read_to_string(self.root.path().join("github.json")).ok()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let item = reference(&format!("{}.github", self.namespace), "token").unwrap();
        let _ = self.store.delete(&item);
    }
}
