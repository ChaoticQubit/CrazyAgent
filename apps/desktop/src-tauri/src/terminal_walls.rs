use super::*;

/// Opens one project's terminal wall windows, one call at a time: builds each of pages 1 to
/// `count` not yet open, retitles the open ones and brings them forward when asked, and
/// closes the project's windows past `count`. Without `focus` a wall is growing: it changes
/// nothing while none of the project's wall windows is open and never closes one, so a
/// growing team only extends a wall the owner opened. Inputs: the app, the project, its
/// shown name, the page count and whether to focus. Output: unit, `terminal_windows_full`
/// past WINDOWS_MAX windows in all, or `terminal_window_failed`.
pub(crate) fn open_walls(
    app: &AppHandle,
    project: &ProjectId,
    name: &str,
    count: usize,
    focus: bool,
) -> Result<(), BridgeError> {
    static WALLS_OPENING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _opening = WALLS_OPENING
        .lock()
        .map_err(|_| engine_failure("terminal_window_failed"))?;
    let prefix = bridge::terminal::wall_prefix(project);
    let open = app.webview_windows();
    let pages: Vec<usize> = open
        .keys()
        .filter_map(|label| label.strip_prefix(&prefix)?.parse().ok())
        .collect();
    let mine = pages.len();
    let count = if focus {
        count
    } else {
        pages.iter().copied().fold(count, usize::max)
    };
    let others = open
        .keys()
        .filter(|label| {
            label.as_str() != "main"
                && !label.starts_with(&prefix)
                && bridge::terminal::window_label_valid(label)
        })
        .count();
    if count > bridge::terminal::WINDOWS_MAX || others + count > bridge::terminal::WINDOWS_MAX {
        return Err(engine_failure("terminal_windows_full"));
    }
    if !focus && mine == 0 {
        return Ok(());
    }
    for (label, wall) in &open {
        let past = label
            .strip_prefix(&prefix)
            .is_some_and(|page| !page.parse::<usize>().is_ok_and(|page| page <= count));
        if past {
            let _ = wall.close();
        }
    }
    for page in 1..=count {
        let label = bridge::terminal::wall_label(project, page);
        let title = format!("{name} \u{b7} Terminals {page} of {count}");
        match open.get(&label) {
            Some(wall) => {
                let _ = wall.set_title(&title);
                if focus {
                    let _ = wall.set_focus();
                }
            }
            None => build_wall(app, &label, project, page, name, &title)?,
        }
    }
    assert!(count <= bridge::terminal::WINDOWS_MAX);
    Ok(())
}
