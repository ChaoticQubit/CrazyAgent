use crazyagent::protocol::*;
use crazyagent::verification::*;

pub(super) fn draft() -> BaselineDraft {
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("project").unwrap(),
        origin: BaselineOrigin::Imported,
        requirements: "Actual queued late delivery fixture".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"real code".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"real integrity".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}
