use super::*;

/// The pinned build's descriptors for every edited entry point, as shipped in its bundle,
/// with neighbors that must stay: Cancel Snooze, Close Remote Connection and a command the
/// build already keeps out of the palette.
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
    r#"super({id:ib.CLOSE_REMOTE_COMMAND_ID,category:e,title:H(18550,"Close Remote Connection"),f1:!0,precondition:D.and(D.or(SC,mF),Jo.negate())})"#,
    "\n",
    r#"super({id:"_manageAccountPreferencesForMcpServer",title:H(6310,"Manage MCP Server Account Preferences"),category:H(6308,"Accounts"),f1:!1})"#,
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
    r#"super({id:s.ID,title:H(170,"Cancel Snooze Inline Suggestions"),precondition:J3o,f1:!0})"#,
    "\n",
    r#"super({id:ib.CLOSE_REMOTE_COMMAND_ID,category:e,title:H(18550,"Close Remote Connection"),f1:!0,precondition:D.and(D.or(SC,mF),Jo.negate())})"#,
    "\n",
    r#"super({id:"_manageAccountPreferencesForMcpServer",title:H(6310,"Manage MCP Server Account Preferences"),category:H(6308,"Accounts"),f1:!1})"#,
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
