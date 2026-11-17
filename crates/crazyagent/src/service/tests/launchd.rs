use super::*;

#[cfg(unix)]
#[test]
fn launchd_plist_runs_the_recorded_executable_with_logs_and_restart() {
    let expected = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
\t<key>Label</key>
\t<string>dev.crazyagent.engine</string>
\t<key>ProgramArguments</key>
\t<array>
\t\t<string>/Applications/CrazyAgent.app/Contents/MacOS/crazyagent</string>
\t\t<string>engine</string>
\t\t<string>--home</string>
\t\t<string>/Users/owner/Library/Application Support/CrazyAgent/engine</string>
\t</array>
\t<key>EnvironmentVariables</key>
\t<dict>
\t\t<key>PATH</key>
\t\t<string>/Users/owner/.local/bin:/opt/homebrew/bin:/usr/bin:/bin</string>
\t\t<key>HOME</key>
\t\t<string>/Users/owner</string>
\t</dict>
\t<key>RunAtLoad</key>
\t<true/>
\t<key>KeepAlive</key>
\t<dict>
\t\t<key>SuccessfulExit</key>
\t\t<false/>
\t</dict>
\t<key>ExitTimeOut</key>
\t<integer>30</integer>
\t<key>ProcessType</key>
\t<string>Interactive</string>
\t<key>StandardOutPath</key>
\t<string>/Users/owner/Library/Application Support/CrazyAgent/engine/logs/engine.log</string>
\t<key>StandardErrorPath</key>
\t<string>/Users/owner/Library/Application Support/CrazyAgent/engine/logs/engine.log</string>
</dict>
</plist>
";
    assert_eq!(
        service(Platform::MacOs).definition_text().unwrap(),
        expected
    );
}

/// A launchd property list gives the engine the HOME of the process that defined the service,
/// escaped as every other path, so a run on a scratch HOME never starts an engine on the
/// owner's real one.
#[test]
fn launchd_plist_gives_the_engine_the_defining_home_escaped() {
    let mut odd = service(Platform::MacOs);
    odd.home = Some(PathBuf::from("/Users/o&<w>"));
    let text = odd.definition_text().unwrap();
    let home = "\t\t<key>HOME</key>\n\t\t<string>/Users/o&amp;&lt;w&gt;</string>\n";
    assert!(text.contains(home), "{text}");
    assert!(!text.contains("o&<"));
}

#[cfg(unix)]
#[test]
fn launchd_plist_escapes_markup_in_paths() {
    let mut odd = service(Platform::MacOs);
    odd.root = PathBuf::from("/Users/o&<w>\"'/engine");
    let text = odd.definition_text().unwrap();
    assert!(text.contains("<string>/Users/o&amp;&lt;w&gt;&quot;&apos;/engine</string>"));
    assert!(!text.contains("o&<"));
}
