use super::{fixture, row};
use crate::error::{SandboxError, invalid, unreadable_reason};
use crate::lines::{
    MODEL_LOOPBACK_WINDOWS_LINE, linux_unbridged_line, model_loopback_windows, refusal_code,
};
use crate::net::SandboxRemote;
use crate::perf_kit::{SAMPLES, time};
use crate::policy::{
    AccessApply, AccessGrant, AgentAccess, AgentFilesRule, AgentNetwork, AgentPolicy, HomeCaches,
    IdeAgentFiles, LevelView, Narrowing, PolicyHooks, ToolCacheScope, ToolchainChoice, ide_network,
    linux_gaps, linux_network_gap, narrowing, recorded_toolchains, windows_network_gap,
};
use crate::settings::SandboxSettings;
use crate::toolchains::{ToolHomes, Toolchain};
use std::hint::black_box;
use std::path::PathBuf;

const ACCESS: [AgentAccess; 3] = [
    AgentAccess::Sandboxed,
    AgentAccess::OwnBranch,
    AgentAccess::Full,
];
const NETWORK: [AgentNetwork; 3] = [AgentNetwork::Open, AgentNetwork::Https, AgentNetwork::None];
const CACHE: [ToolCacheScope; 2] = [ToolCacheScope::Shared, ToolCacheScope::PerAgent];
const HOME_CACHES: [HomeCaches; 3] = [
    HomeCaches::Keep,
    HomeCaches::ClearWhenRaised,
    HomeCaches::ClearEveryLaunch,
];
const AGENT_FILES: [IdeAgentFiles; 3] = [
    IdeAgentFiles::AllowAll,
    IdeAgentFiles::NoDeleteOrRename,
    IdeAgentFiles::ReadOnly,
];
const APPLY: [AccessApply; 2] = [AccessApply::WhenIdle, AccessApply::NextStart];
const NARROWING: [Narrowing; 6] = [
    Narrowing::Access,
    Narrowing::Network,
    Narrowing::TurnedOff,
    Narrowing::NotAllowed,
    Narrowing::PolicyUnreadable,
    Narrowing::FolderUnreadable,
];
const MODEL: SandboxRemote = SandboxRemote::AnyHost(443);

/// The view's project folders as the engine reads them, through the stand-in access hooks.
const HOOKS: PolicyHooks = PolicyHooks {
    local_folders: |root| crate::shared_folders::local_folders(&crate::test_hooks::ACCESS, root),
};

/// A saved policy with own branch access, Rust allowed, HTTPS network and two grants, which
/// passes its own check. Output: the policy.
fn saved() -> AgentPolicy {
    let policy = AgentPolicy {
        access: AgentAccess::OwnBranch,
        toolchains: vec![ToolchainChoice {
            toolchain: Toolchain::Rust,
            allowed: true,
        }],
        network: AgentNetwork::Https,
        grants: vec![AccessGrant::Port { port: 3000 }, AccessGrant::TempFolder],
        ..AgentPolicy::default()
    };
    assert_eq!(policy.validate(), Ok(()));
    policy
}

/// The three grant shapes, each plain. Output: the grants.
fn grants() -> [AccessGrant; 3] {
    let grants = [
        AccessGrant::Socket {
            path: PathBuf::from("/tmp/perf-agent.sock"),
        },
        AccessGrant::Port { port: 3000 },
        AccessGrant::TempFolder,
    ];
    assert!(grants.iter().all(AccessGrant::plain));
    grants
}

/// Times the sandbox's free name and error calls: the confinement table, the unreadable
/// line, a refusal, the Windows loopback check, the unbridged line, the refusal code and a
/// model endpoint's remote. Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn names_and_errors() {
    let errors: [SandboxError; 2] = [
        invalid("perf refusal"),
        SandboxError::Unavailable(MODEL_LOOPBACK_WINDOWS_LINE.into()),
    ];
    assert!(model_loopback_windows(&errors[1]) && !model_loopback_windows(&errors[0]));
    let folder = PathBuf::from("/Users/perf/project");
    time(&row("confinement.confinement", 1000), SAMPLES, || {
        black_box(crate::confinement());
    });
    time(&row("error.unreadable_reason", 100), SAMPLES, || {
        black_box(unreadable_reason(black_box(&folder)));
    });
    time(&row("error.invalid", 1000), SAMPLES, || {
        black_box(invalid(black_box("perf refusal")));
    });
    let mut at = 0usize;
    time(&row("lines.model_loopback_windows", 1000), SAMPLES, || {
        at += 1;
        black_box(model_loopback_windows(black_box(&errors[at % 2])));
    });
    time(&row("lines.linux_unbridged_line", 100), SAMPLES, || {
        black_box(linux_unbridged_line(black_box(22)));
    });
    time(&row("lines.refusal_code", 1000), SAMPLES, || {
        at += 1;
        black_box(refusal_code(black_box(&errors[at % 2])));
    });
    time(&row("net.SandboxRemote.for_endpoint", 100), SAMPLES, || {
        let _ = black_box(SandboxRemote::for_endpoint(black_box(
            "https://api.anthropic.com/v1",
        )));
    });
}

/// Times the access, network, tool cache, home cache, agent files and apply choices: each
/// call runs over every variant it accepts in turn, so one row stands for the whole enum.
/// Reach gets a proxy port only where the choice is proxied, and the IDE network skips Full,
/// which never asks. Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn choices() {
    let mut at = 0usize;
    let mut next = || {
        at += 1;
        at
    };
    time(&row("policy.AgentAccess.id", 1000), SAMPLES, || {
        black_box(ACCESS[next() % 3].id());
    });
    time(&row("policy.AgentAccess.wider_than", 1000), SAMPLES, || {
        let step = next();
        black_box(ACCESS[step % 3].wider_than(ACCESS[(step / 3) % 3]));
    });
    time(&row("policy.AgentAccess.label", 1000), SAMPLES, || {
        black_box(ACCESS[next() % 3].label());
    });
    time(&row("policy.AgentAccess.refusal", 1000), SAMPLES, || {
        black_box(ACCESS[next() % 3].refusal());
    });
    time(
        &row("policy.AgentAccess.unavailable", 1000),
        SAMPLES,
        || {
            black_box(ACCESS[next() % 3].unavailable());
        },
    );
    time(&row("policy.AgentNetwork.id", 1000), SAMPLES, || {
        black_box(NETWORK[next() % 3].id());
    });
    time(&row("policy.AgentNetwork.label", 1000), SAMPLES, || {
        black_box(NETWORK[next() % 3].label());
    });
    time(
        &row("policy.AgentNetwork.wider_than", 1000),
        SAMPLES,
        || {
            let step = next();
            black_box(NETWORK[step % 3].wider_than(NETWORK[(step / 3) % 3]));
        },
    );
    time(&row("policy.AgentNetwork.proxied", 1000), SAMPLES, || {
        black_box(NETWORK[next() % 3].proxied(black_box("macos"), true, MODEL));
    });
    let reaches = NETWORK.map(|network| {
        (
            network,
            network.proxied("macos", true, MODEL).then_some(8080),
        )
    });
    time(&row("policy.AgentNetwork.reach", 100), SAMPLES, || {
        let (network, proxy) = reaches[next() % 3];
        let _ = black_box(network.reach(black_box("macos"), true, MODEL, proxy));
    });
    time(&row("policy.AgentNetwork.sandbox", 100), SAMPLES, || {
        black_box(NETWORK[next() % 3].sandbox(Some(MODEL)));
    });
    time(&row("policy.ToolCacheScope.id", 1000), SAMPLES, || {
        black_box(CACHE[next() % 2].id());
    });
    time(&row("policy.ToolCacheScope.label", 1000), SAMPLES, || {
        black_box(CACHE[next() % 2].label());
    });
    time(&row("policy.HomeCaches.id", 1000), SAMPLES, || {
        black_box(HOME_CACHES[next() % 3].id());
    });
    time(&row("policy.HomeCaches.label", 1000), SAMPLES, || {
        black_box(HOME_CACHES[next() % 3].label());
    });
    time(&row("policy.ide_network", 100), SAMPLES, || {
        let step = next();
        black_box(ide_network(ACCESS[step % 2], NETWORK[(step / 2) % 3]));
    });
    time(&row("policy.IdeAgentFiles.label", 1000), SAMPLES, || {
        black_box(AGENT_FILES[next() % 3].label());
    });
    time(&row("policy.AccessApply.id", 1000), SAMPLES, || {
        black_box(APPLY[next() % 2].id());
    });
    time(&row("policy.AccessApply.label", 1000), SAMPLES, || {
        black_box(APPLY[next() % 2].label());
    });
    time(&row("policy.Narrowing.code", 1000), SAMPLES, || {
        black_box(NARROWING[next() % 6].code());
    });
}

/// Times the grant, agent files rule, gap and level calls that take no project: plain and
/// label over the three grant shapes, the agent files rule text over its three choices, the
/// Linux and Windows network gaps, the Linux gaps and the level views with no Linux host.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn grants_gaps_and_levels() {
    let grants = grants();
    let root = PathBuf::from("/Users/perf/project");
    let mut at = 0usize;
    let mut next = || {
        at += 1;
        at
    };
    time(&row("policy.AccessGrant.plain", 1000), SAMPLES, || {
        black_box(grants[next() % 3].plain());
    });
    time(&row("policy.AccessGrant.label", 100), SAMPLES, || {
        black_box(grants[next() % 3].label());
    });
    time(&row("policy.AgentFilesRule.seatbelt", 100), SAMPLES, || {
        let step = next();
        let rule = AgentFilesRule {
            choice: AGENT_FILES[step % 3],
            older_agents: step % 2 == 0,
        };
        black_box(rule.seatbelt(black_box(&root)));
    });
    time(&row("policy.linux_network_gap", 100), SAMPLES, || {
        black_box(linux_network_gap(NETWORK[next() % 3], None, None, &grants));
    });
    time(&row("policy.windows_network_gap", 1000), SAMPLES, || {
        black_box(windows_network_gap(
            black_box("windows"),
            NETWORK[next() % 3],
        ));
    });
    time(&row("policy.LevelView.all", 100), SAMPLES, || {
        black_box(LevelView::all(None, None));
    });
    time(&row("policy.LevelView.on", 100), SAMPLES, || {
        black_box(LevelView::on(None, next() % 2 == 0));
    });
    time(&row("policy.LevelView.other_git", 1000), SAMPLES, || {
        black_box(LevelView::other_git(None, black_box(&root), |_| false));
    });
    time(&row("policy.linux_gaps", 100), SAMPLES, || {
        black_box(linux_gaps(ACCESS[next() % 3], None, true));
    });
}

/// Times the saved policy calls on a temp project with Rust and Node manifests and a temp
/// owner home: the check, the allowed toolchains, the pinned copy, the client view, a widen
/// and a revoke test, the narrowing a save meets, and the toolchains a new project records.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn saved_policy() {
    let (_temp, project, home) = fixture();
    let homes = ToolHomes::at(&home);
    let policy = saved();
    let wider = [Toolchain::Rust, Toolchain::Node];
    let grants = grants();
    assert_eq!(policy.allowed(&project), wider.to_vec());
    time(&row("policy.AgentPolicy.validate", 1000), SAMPLES, || {
        let _ = black_box(black_box(&policy).validate());
    });
    time(&row("policy.AgentPolicy.allowed", 10), SAMPLES, || {
        black_box(policy.allowed(black_box(&project)));
    });
    time(&row("policy.AgentPolicy.pinned", 10), SAMPLES, || {
        black_box(policy.pinned(black_box(&project)));
    });
    time(&row("policy.AgentPolicy.view", 10), SAMPLES, || {
        black_box(policy.view(
            &HOOKS,
            None,
            black_box(&project),
            &homes,
            7,
            Some(SandboxSettings::default()),
            false,
        ));
    });
    time(&row("policy.AgentPolicy.widens", 10), SAMPLES, || {
        black_box(policy.widens(
            Some(black_box(&project)),
            Some(AgentAccess::Full),
            Some(AgentNetwork::Open),
            Some(&wider),
            &grants,
        ));
    });
    time(&row("policy.AgentPolicy.revokes", 1000), SAMPLES, || {
        black_box(policy.revokes(black_box(&grants[1..])));
    });
    time(&row("policy.narrowing", 10), SAMPLES, || {
        black_box(narrowing(
            Some(&policy),
            Some(black_box(&project)),
            Some(AgentAccess::Sandboxed),
            Some(AgentNetwork::None),
            &[Toolchain::Rust],
        ));
    });
    time(&row("policy.recorded_toolchains", 10), SAMPLES, || {
        black_box(recorded_toolchains(black_box(&project), Some(&homes)));
    });
}
