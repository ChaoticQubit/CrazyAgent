use super::InstallError;
use super::process::{private_folder, staging_folder};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// Every file of the owner-authored skills CrazyAgent ships under Apache-2.0, as a path
/// relative to `assets/pack` and its bytes, compiled into the engine so setup needs no
/// source checkout.
pub const FILES: &[(&str, &[u8])] = &[
    (
        "skills/deep-docs/diagram-codebase/SKILL.md",
        include_bytes!("../../assets/pack/skills/deep-docs/diagram-codebase/SKILL.md"),
    ),
    (
        "skills/deep-docs/document-product/SKILL.md",
        include_bytes!("../../assets/pack/skills/deep-docs/document-product/SKILL.md"),
    ),
    (
        "skills/deep-docs/document-product/assets/report-skeleton.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/assets/report-skeleton.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/discovery-playbook.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/discovery-playbook.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/documentation-outline.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/documentation-outline.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/dsl-documentation-guide.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/dsl-documentation-guide.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/mermaid-cookbook.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/mermaid-cookbook.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/setup-and-cicd.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/setup-and-cicd.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/stakeholder-views.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/stakeholder-views.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/status-and-gaps.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/status-and-gaps.md"
        ),
    ),
    (
        "skills/deep-docs/document-product/references/testing-and-debugging.md",
        include_bytes!(
            "../../assets/pack/skills/deep-docs/document-product/references/testing-and-debugging.md"
        ),
    ),
    (
        "skills/html-explainer/SKILL.md",
        include_bytes!("../../assets/pack/skills/html-explainer/SKILL.md"),
    ),
    (
        "skills/html-explainer/assets/skeleton.html",
        include_bytes!("../../assets/pack/skills/html-explainer/assets/skeleton.html"),
    ),
    (
        "skills/html-explainer/references/patterns.md",
        include_bytes!("../../assets/pack/skills/html-explainer/references/patterns.md"),
    ),
    (
        "skills/linear-method/SKILL.md",
        include_bytes!("../../assets/pack/skills/linear-method/SKILL.md"),
    ),
    (
        "skills/security-audit/AI-AND-LLM.md",
        include_bytes!("../../assets/pack/skills/security-audit/AI-AND-LLM.md"),
    ),
    (
        "skills/security-audit/ATTACK-CLASSES.md",
        include_bytes!("../../assets/pack/skills/security-audit/ATTACK-CLASSES.md"),
    ),
    (
        "skills/security-audit/CLIENT-SIDE.md",
        include_bytes!("../../assets/pack/skills/security-audit/CLIENT-SIDE.md"),
    ),
    (
        "skills/security-audit/CLOUD-AND-DEPLOYMENT.md",
        include_bytes!("../../assets/pack/skills/security-audit/CLOUD-AND-DEPLOYMENT.md"),
    ),
    (
        "skills/security-audit/DATA-ISOLATION-AND-LIFECYCLE.md",
        include_bytes!("../../assets/pack/skills/security-audit/DATA-ISOLATION-AND-LIFECYCLE.md"),
    ),
    (
        "skills/security-audit/DESKTOP-MOBILE-AND-LOCAL-IPC.md",
        include_bytes!("../../assets/pack/skills/security-audit/DESKTOP-MOBILE-AND-LOCAL-IPC.md"),
    ),
    (
        "skills/security-audit/HUNTING.md",
        include_bytes!("../../assets/pack/skills/security-audit/HUNTING.md"),
    ),
    (
        "skills/security-audit/MEMORY-SAFETY-AND-BINARY.md",
        include_bytes!("../../assets/pack/skills/security-audit/MEMORY-SAFETY-AND-BINARY.md"),
    ),
    (
        "skills/security-audit/PROTOCOLS-RPC-AND-MESSAGING.md",
        include_bytes!("../../assets/pack/skills/security-audit/PROTOCOLS-RPC-AND-MESSAGING.md"),
    ),
    (
        "skills/security-audit/RECONNAISSANCE.md",
        include_bytes!("../../assets/pack/skills/security-audit/RECONNAISSANCE.md"),
    ),
    (
        "skills/security-audit/RESOURCE-EXHAUSTION-AND-AVAILABILITY.md",
        include_bytes!(
            "../../assets/pack/skills/security-audit/RESOURCE-EXHAUSTION-AND-AVAILABILITY.md"
        ),
    ),
    (
        "skills/security-audit/SKILL.md",
        include_bytes!("../../assets/pack/skills/security-audit/SKILL.md"),
    ),
    (
        "skills/security-audit/SUPPLY-CHAIN-AND-RELEASE.md",
        include_bytes!("../../assets/pack/skills/security-audit/SUPPLY-CHAIN-AND-RELEASE.md"),
    ),
    (
        "skills/security-audit/VALIDATION-AND-REPORTING.md",
        include_bytes!("../../assets/pack/skills/security-audit/VALIDATION-AND-REPORTING.md"),
    ),
    (
        "skills/security-audit/WEB-PROTOCOL-AND-AUTH.md",
        include_bytes!("../../assets/pack/skills/security-audit/WEB-PROTOCOL-AND-AUTH.md"),
    ),
    (
        "skills/security-audit/report-schema.json",
        include_bytes!("../../assets/pack/skills/security-audit/report-schema.json"),
    ),
    (
        "skills/security-audit/validate-coverage-ledger.cjs",
        include_bytes!("../../assets/pack/skills/security-audit/validate-coverage-ledger.cjs"),
    ),
    (
        "skills/security-audit/validate-coverage-ledger.test.cjs",
        include_bytes!("../../assets/pack/skills/security-audit/validate-coverage-ledger.test.cjs"),
    ),
    (
        "skills/security-audit/validate-findings.cjs",
        include_bytes!("../../assets/pack/skills/security-audit/validate-findings.cjs"),
    ),
    (
        "skills/security-audit/validate-findings.test.cjs",
        include_bytes!("../../assets/pack/skills/security-audit/validate-findings.test.cjs"),
    ),
    (
        "skills/software-practices/code-review/SKILL.md",
        include_bytes!("../../assets/pack/skills/software-practices/code-review/SKILL.md"),
    ),
    (
        "skills/software-practices/code-review/references/review-checklist.md",
        include_bytes!(
            "../../assets/pack/skills/software-practices/code-review/references/review-checklist.md"
        ),
    ),
    (
        "skills/software-practices/engineering-principles/SKILL.md",
        include_bytes!(
            "../../assets/pack/skills/software-practices/engineering-principles/SKILL.md"
        ),
    ),
    (
        "skills/software-practices/feature-flags/SKILL.md",
        include_bytes!("../../assets/pack/skills/software-practices/feature-flags/SKILL.md"),
    ),
    (
        "skills/software-practices/feature-flags/references/flag-patterns.md",
        include_bytes!(
            "../../assets/pack/skills/software-practices/feature-flags/references/flag-patterns.md"
        ),
    ),
    (
        "skills/software-practices/testing/SKILL.md",
        include_bytes!("../../assets/pack/skills/software-practices/testing/SKILL.md"),
    ),
    (
        "skills/software-practices/testing/references/testing-patterns.md",
        include_bytes!(
            "../../assets/pack/skills/software-practices/testing/references/testing-patterns.md"
        ),
    ),
    (
        "skills/software-practices/trunk-based-development/SKILL.md",
        include_bytes!(
            "../../assets/pack/skills/software-practices/trunk-based-development/SKILL.md"
        ),
    ),
    (
        "skills/software-practices/trunk-based-development/references/branching-playbook.md",
        include_bytes!(
            "../../assets/pack/skills/software-practices/trunk-based-development/references/branching-playbook.md"
        ),
    ),
    (
        "skills/tiger-style/SKILL.md",
        include_bytes!("../../assets/pack/skills/tiger-style/SKILL.md"),
    ),
    (
        "skills/to-features/SKILL.md",
        include_bytes!("../../assets/pack/skills/to-features/SKILL.md"),
    ),
    (
        "skills/to-linear-projects/SKILL.md",
        include_bytes!("../../assets/pack/skills/to-linear-projects/SKILL.md"),
    ),
    (
        "skills/to-releases/SKILL.md",
        include_bytes!("../../assets/pack/skills/to-releases/SKILL.md"),
    ),
];

/// Fingerprints the shipped files under one bundled path, so a new app version with
/// changed skill text installs again. Input: a bundled path such as `skills/tiger-style`.
/// Output: the lowercase hex SHA-256 over each file's path and bytes, in order.
pub fn digest(path: &str) -> String {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    let mut hasher = Sha256::new();
    for (file, bytes) in FILES.iter().filter(|(file, _)| file.starts_with(&prefix)) {
        hasher.update((file.len() as u64).to_be_bytes());
        hasher.update(file.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    let text: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(text.len(), 64);
    text
}

/// Writes the shipped skills into `<setup>/bundled`, replacing any older copy, with
/// folders 0700 and files 0600 owned by this user. Input: the setup folder. Output: the
/// bundled folder, the root that bundled pack paths such as `skills/tiger-style` name.
pub fn extract(setup: &Path) -> Result<PathBuf, InstallError> {
    private_folder(setup)?;
    let target = setup.join("bundled");
    let staging = staging_folder(setup, "bundled")?;
    let result = (|| {
        for (path, bytes) in FILES {
            let relative = Path::new(path);
            assert!(
                relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            );
            let file = staging.join(relative);
            if let Some(parent) = file.parent() {
                private_folder(parent)?;
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&file)
                .and_then(|mut handle| handle.write_all(bytes))
                .map_err(|_| InstallError("setup_store_unavailable"))?;
        }
        if std::fs::symlink_metadata(&target).is_ok() {
            std::fs::remove_dir_all(&target)
                .map_err(|_| InstallError("setup_store_unavailable"))?;
        }
        std::fs::rename(&staging, &target).map_err(|_| InstallError("setup_store_unavailable"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map(|()| target)
}

#[cfg(test)]
#[path = "bundled_tests.rs"]
mod tests;
