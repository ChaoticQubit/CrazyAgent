use super::*;

/// The pinned build's text at every edit, as shipped in its bundle (long preconditions and
/// lists cut short), with neighbors that must stay: Close Remote Connection, a command the
/// build already keeps out of the palette, and the Window node of the Settings contents.
pub(crate) const SHIPPED: &str = concat!(
    r#"super({id:ib.REMOTE_ACTIONS_COMMAND_ID,category:e,title:H(18553,"Show Remote Menu"),f1:!0,keybinding:{weight:200,primary:2605}})"#,
    "\n",
    r#"super({id:ib.INSTALL_REMOTE_EXTENSIONS_ID,category:e,title:H(18551,"Install Remote Development Extensions"),f1:!0})"#,
    "\n",
    r#"super({id:"workbench.action.syncAccountPolicy",title:H(4210,"Sync Account Policy"),category:Ze.Developer,f1:!0})"#,
    "\n",
    r#"super({id:s.ID,title:H(171,"Snooze Inline Suggestions"),precondition:D.true(),f1:!0})"#,
    "\n",
    r#"super({id:"workbench.action.manageAccounts",title:H(6320,"Manage Accounts"),category:H(6318,"Accounts"),f1:!0})"#,
    "\n",
    r#"super({id:"_manageTrustedExtensionsForAccount",title:H(6343,"Manage Trusted Extensions For Account"),category:H(6339,"Accounts"),f1:!0})"#,
    "\n",
    r#"super({id:"_manageAccountPreferencesForExtension",title:H(6299,"Manage Extension Account Preferences..."),category:H(6297,"Accounts"),f1:!0,menu:[{id:F.AccountsContext,order:100}]})"#,
    "\n",
    r#"super({id:"_manageTrustedMCPServersForAccount",title:H(6357,"Manage Trusted MCP Servers For Account"),category:H(6352,"Accounts"),f1:!0,precondition:D.and(te.Setup.hidden.negate(),te.Setup.disabledInWorkspace.negate())})"#,
    "\n",
    r#"this.renderRemoteStatusIndicator(ib.DEFAULT_REMOTE_STATUS_LABEL,d(18547,null))}renderRemoteStatusIndicator(e,t,i,n){let{text:r,tooltip:a,ariaLabel:c}=this.withNetworkS"#,
    "\n",
    r#"super({id:s.ID,title:H(170,"Cancel Snooze Inline Suggestions"),precondition:J3o,f1:!0})"#,
    "\n",
    r#"super({id:s.ID,title:H(6335,"Remove Dynamic Authentication Providers"),category:H(6327,"Authentication"),f1:!0})"#,
    "\n",
    r#"super({id:"workbench.action.showPolicyDiagnostics",title:H(4180,"Policy Diagnostics"),category:Ze.Developer,f1:!0})"#,
    "\n",
    r#"super({id:"workbench.action.chat.manageSettings",title:H(6728,"Manage Copilot Settings"),category:zi,f1:!0,precondition:D.and(D.or(te.Entitlement.planFree,te.Entitlement.planEdu),o)})"#,
    "\n",
    r#"super({id:"workbench.action.chat.showExtensionsUsingCopilot",title:H(6737,"Show Extensions using Copilot"),f1:!0,category:RZ,precondition:te.enabled})"#,
    "\n",
    r#"super({id:"workbench.action.chat.upgradePlan",title:H(10647,"Upgrade to GitHub Copilot Pro"),category:H(10640,"Chat"),f1:!0,precondition:D.has("config.chat.disableAIFeatures").negate()})"#,
    "\n",
    r#"super({id:"workbench.action.chat.manageAdditionalSpend",title:H(10646,"Manage GitHub Copilot Budget"),category:H(10640,"Chat"),f1:!0,precondition:D.has("config.chat.disableAIFeatures").negate()})"#,
    "\n",
    r#"run:()=>{throw new Error('"Manage" can not be hidden')}})]}function gxt(s){return s.getBoolean(c$.ACCOUNTS_VISIBILITY_PREFERENCE_KEY,0,!0)}function Nli(s,o){s.store(c$.ACCOUNTS_VISIBILITY_PREFERENCE_KEY,o,0,0)}"#,
    "\n",
    r#"{id:"window",label:d(18304,null),settings:["window.*"],children:[{id:"window/newWindow",label:d(18283,null),settings:["window.*newwindow*"]}]},{id:"chat",label:d(18254,null),children:[{id:"chat/agent",label:d(18255,null),settings:["chat.agent.*"]}]}"#,
    "\n",
    r#"super({id:ib.CLOSE_REMOTE_COMMAND_ID,category:e,title:H(18550,"Close Remote Connection"),f1:!0,precondition:D.and(D.or(SC,mF),Jo.negate())})"#,
    "\n",
    r#"super({id:"_manageAccountPreferencesForMcpServer",title:H(6310,"Manage MCP Server Account Preferences"),category:H(6308,"Accounts"),f1:!1})"#,
    "\n",
    r#"Xs({id:"workbench.action.terminal.selectDefaultShell",title:H(19950,"Select Default Profile"),run:o=>o.service.showProfileQuickPick("setDefault")})"#,
    "\n",
    r#"{id:F.TerminalNewDropdownContext,item:{command:{id:"workbench.action.terminal.selectDefaultShell",title:H(20028,"Select Default Profile")},group:"3_configure"}}"#,
    "\n",
    r#"_createProfileQuickPickItem(o){let e=[{iconClass:J.asClassName(KMn),tooltip:d(20038,null)}],i=o.icon}"#,
    "\n",
);

/// [`SHIPPED`] once every edit applied.
pub(crate) const HIDDEN: &str = concat!(
    r#"super({id:ib.REMOTE_ACTIONS_COMMAND_ID,category:e,title:H(18553,"Show Remote Menu"),f1:!1})"#,
    "\n",
    r#"super({id:ib.INSTALL_REMOTE_EXTENSIONS_ID,category:e,title:H(18551,"Install Remote Development Extensions"),f1:!1})"#,
    "\n",
    r#"super({id:"workbench.action.syncAccountPolicy",title:H(4210,"Sync Account Policy"),category:Ze.Developer,f1:!1})"#,
    "\n",
    r#"super({id:s.ID,title:H(171,"Snooze Inline Suggestions"),precondition:D.true(),f1:!1})"#,
    "\n",
    r#"super({id:"workbench.action.manageAccounts",title:H(6320,"Manage Accounts"),category:H(6318,"Accounts"),f1:!1})"#,
    "\n",
    r#"super({id:"_manageTrustedExtensionsForAccount",title:H(6343,"Manage Trusted Extensions For Account"),category:H(6339,"Accounts"),f1:!1})"#,
    "\n",
    r#"super({id:"_manageAccountPreferencesForExtension",title:H(6299,"Manage Extension Account Preferences..."),category:H(6297,"Accounts"),f1:!1,menu:[{id:F.AccountsContext,order:100}]})"#,
    "\n",
    r#"super({id:"_manageTrustedMCPServersForAccount",title:H(6357,"Manage Trusted MCP Servers For Account"),category:H(6352,"Accounts"),f1:!1,precondition:D.and(te.Setup.hidden.negate(),te.Setup.disabledInWorkspace.negate())})"#,
    "\n",
    r#"this.renderRemoteStatusIndicator(ib.DEFAULT_REMOTE_STATUS_LABEL,d(18547,null))}renderRemoteStatusIndicator(e,t,i,n){return;let{text:r,tooltip:a,ariaLabel:c}=this.withNetworkS"#,
    "\n",
    r#"super({id:s.ID,title:H(170,"Cancel Snooze Inline Suggestions"),precondition:J3o,f1:!1})"#,
    "\n",
    r#"super({id:s.ID,title:H(6335,"Remove Dynamic Authentication Providers"),category:H(6327,"Authentication"),f1:!1})"#,
    "\n",
    r#"super({id:"workbench.action.showPolicyDiagnostics",title:H(4180,"Policy Diagnostics"),category:Ze.Developer,f1:!1})"#,
    "\n",
    r#"super({id:"workbench.action.chat.manageSettings",title:H(6728,"Manage Copilot Settings"),category:zi,f1:!1,precondition:D.and(D.or(te.Entitlement.planFree,te.Entitlement.planEdu),o)})"#,
    "\n",
    r#"super({id:"workbench.action.chat.showExtensionsUsingCopilot",title:H(6737,"Show Extensions using Copilot"),f1:!1,category:RZ,precondition:te.enabled})"#,
    "\n",
    r#"super({id:"workbench.action.chat.upgradePlan",title:H(10647,"Upgrade to GitHub Copilot Pro"),category:H(10640,"Chat"),f1:!1,precondition:D.has("config.chat.disableAIFeatures").negate()})"#,
    "\n",
    r#"super({id:"workbench.action.chat.manageAdditionalSpend",title:H(10646,"Manage GitHub Copilot Budget"),category:H(10640,"Chat"),f1:!1,precondition:D.has("config.chat.disableAIFeatures").negate()})"#,
    "\n",
    r#"run:()=>{throw new Error('"Manage" can not be hidden')}})]}function gxt(s){return!1&&s.getBoolean(c$.ACCOUNTS_VISIBILITY_PREFERENCE_KEY,0,!0)}function Nli(s,o){s.store(c$.ACCOUNTS_VISIBILITY_PREFERENCE_KEY,o,0,0)}"#,
    "\n",
    r#"{id:"window",label:d(18304,null),settings:["window.*"],children:[{id:"window/newWindow",label:d(18283,null),settings:["window.*newwindow*"]}]},{id:"chat",label:d(18254,null),hide:!0,children:[{id:"chat/agent",label:d(18255,null),settings:["chat.agent.*"]}]}"#,
    "\n",
    r#"super({id:ib.CLOSE_REMOTE_COMMAND_ID,category:e,title:H(18550,"Close Remote Connection"),f1:!0,precondition:D.and(D.or(SC,mF),Jo.negate())})"#,
    "\n",
    r#"super({id:"_manageAccountPreferencesForMcpServer",title:H(6310,"Manage MCP Server Account Preferences"),category:H(6308,"Accounts"),f1:!1})"#,
    "\n",
    r#"Xs({id:"workbench.action.terminal.selectDefaultShell",title:H(19950,"Select Default Profile"),f1:!1,run:o=>o.service.showProfileQuickPick("setDefault")})"#,
    "\n",
    r#"{id:F.TerminalNewDropdownContext,item:{command:{id:"workbench.action.terminal.selectDefaultShell",title:H(20028,"Select Default Profile")},when:D.false(),group:"3_configure"}}"#,
    "\n",
    r#"_createProfileQuickPickItem(o){let e=[],i=o.icon}"#,
    "\n",
);

fn bundle_at(root: &Path, text: &str) -> std::path::PathBuf {
    let path = root.join(BUNDLE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

fn refused(root: &Path) -> String {
    apply(root).unwrap_err().0
}

fn staged_left(root: &Path) -> bool {
    root.join(BUNDLE).with_extension("js.hiding").exists()
}

#[test]
fn the_pinned_build_s_removed_parts_leave_the_palette_and_the_status_bar_and_the_rest_stays() {
    let tmp = tempfile::tempdir().unwrap();
    let path = bundle_at(tmp.path(), SHIPPED);
    apply(tmp.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HIDDEN);
    assert!(!staged_left(tmp.path()));
    assert_eq!(hide(SHIPPED).as_deref(), Some(HIDDEN));
}

#[test]
fn a_bundle_unlike_the_pinned_build_is_refused_and_left_as_it_was() {
    let missing = SHIPPED.replace(r#""Sync Account Policy")"#, r#""Sync Policy")"#);
    let twice = format!(
        "{SHIPPED}{}",
        r#"super({id:"workbench.action.manageAccounts",title:H(6320,"Manage Accounts"),category:H(6318,"Accounts"),f1:!0})"#
    );
    let far = SHIPPED.replace(
        "precondition:D.true(),f1:!0",
        "precondition:D.and(D.true(),D.true(),D.true()),f1:!0",
    );
    let changed = SHIPPED.replace(
        "f1:!0,keybinding:{weight:200,primary:2605}",
        "f1:!0,keybinding:{weight:200,primary:2606}",
    );
    let renamed = SHIPPED.replace("}renderRemoteStatusIndicator(", "}renderRemoteIndicator(");
    for (case, text) in [
        ("missing", missing),
        ("twice", twice),
        ("far", far),
        ("changed", changed),
        ("renamed", renamed),
    ] {
        assert_ne!(text, SHIPPED, "{case}");
        let tmp = tempfile::tempdir().unwrap();
        let path = bundle_at(tmp.path(), &text);
        assert_eq!(refused(tmp.path()), "ide_build_patch_failed", "{case}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{case}");
        assert!(!staged_left(tmp.path()), "{case}");
    }
}

#[test]
fn a_build_whose_bundle_is_missing_oversized_or_linked_is_refused() {
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(refused(empty.path()), "ide_build_patch_failed");
    let large = tempfile::tempdir().unwrap();
    let path = bundle_at(large.path(), SHIPPED);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(BUNDLE_BYTES_MAX + 1)
        .unwrap();
    assert_eq!(refused(large.path()), "ide_build_patch_failed");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        BUNDLE_BYTES_MAX + 1
    );
    assert!(!staged_left(large.path()));
    #[cfg(unix)]
    {
        let linked = tempfile::tempdir().unwrap();
        let target = linked.path().join("real.js");
        std::fs::write(&target, SHIPPED).unwrap();
        let path = linked.path().join(BUNDLE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(refused(linked.path()), "ide_build_patch_failed");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), SHIPPED);
        assert!(!staged_left(linked.path()));
    }
}

#[test]
#[cfg(unix)]
fn a_link_or_leftover_at_the_staged_name_is_replaced_and_never_followed() {
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("outside.js");
    std::fs::write(&target, "keep").unwrap();
    let linked = tempfile::tempdir().unwrap();
    let path = bundle_at(linked.path(), SHIPPED);
    std::os::unix::fs::symlink(&target, path.with_extension("js.hiding")).unwrap();
    apply(linked.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HIDDEN);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    assert!(!staged_left(linked.path()));
    assert!(
        std::fs::symlink_metadata(path.with_extension("js.hiding")).is_err(),
        "the planted link is gone"
    );

    let leftover = tempfile::tempdir().unwrap();
    let path = bundle_at(leftover.path(), SHIPPED);
    std::fs::write(path.with_extension("js.hiding"), "half a bundle").unwrap();
    apply(leftover.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HIDDEN);
    assert!(!staged_left(leftover.path()));

    let folder = tempfile::tempdir().unwrap();
    let path = bundle_at(folder.path(), SHIPPED);
    std::fs::create_dir(path.with_extension("js.hiding")).unwrap();
    assert_eq!(refused(folder.path()), "ide_build_patch_failed");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SHIPPED);
}

/// Every hiding revision, oldest first, with the digest of the edits a build installed under
/// it carries. Changing [`EDITS`] or [`WINDOW`] changes the digest, so it needs a new row
/// and a new [`REVISION`], or installed builds would keep the old edits.
const REVISIONS: [(&str, &str); 4] = [
    (
        "h1",
        "257622519e4a1a7d3a7bee30f97186083c9a4e14b1f5cdaaf83e3ef33347430b",
    ),
    (
        "h2",
        "794fd53ff124982ffacd3a829147c6f969f1df4694efa015a78fb4d62d9c22dc",
    ),
    (
        "h3",
        "97a8ce21fc87e75d1d93eb37df327c3b7bacc2e7539634e94e383a5a3e906984",
    ),
    (
        "h4",
        "662587e356846db83b228b5b64e84472d8c5e74496a69408c614f782d6e99eb4",
    ),
];

/// The sha256 of the window and each edit's anchor, from and to. Input: the edits. Output:
/// the lowercase hex digest.
fn digest(edits: &[Edit]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("{WINDOW}\n"));
    for edit in edits {
        hasher.update(format!("{}\0{}\0{}\n", edit.anchor, edit.from, edit.to));
    }
    format!("{:x}", hasher.finalize())
}

#[test]
fn every_change_to_the_edits_names_a_new_revision() {
    let current = digest(&EDITS);
    assert_eq!(REVISIONS.last(), Some(&(REVISION, current.as_str())));
    assert_eq!(digest(&EDITS[..9]), REVISIONS[0].1);
    assert_eq!(digest(&EDITS[..16]), REVISIONS[1].1);
    assert_eq!(digest(&EDITS[..18]), REVISIONS[2].1);
    for (n, (name, sum)) in REVISIONS.iter().enumerate() {
        for (other, other_sum) in &REVISIONS[n + 1..] {
            assert_ne!(name, other);
            assert_ne!(sum, other_sum);
        }
    }
}
