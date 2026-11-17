use std::sync::{Mutex, PoisonError};

use serde::Deserialize;
#[cfg(any(windows, test))]
use tauri::image::Image;
use tauri::{AppHandle, Manager, Runtime, Theme, WebviewWindow};

/// The appearance the owner picked in the app's Profile settings: follow the system, or a
/// fixed light or dark. Input: the word the webview keeps (`system`, `light` or `dark`).
/// Output: the choice; any other word is refused when a command reads it.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    /// The theme every window is held to. Output: none for System, so the windows follow the
    /// system appearance; the fixed theme otherwise.
    pub fn held(self) -> Option<Theme> {
        match self {
            Self::System => None,
            Self::Light => Some(Theme::Light),
            Self::Dark => Some(Theme::Dark),
        }
    }

    /// The icon that matches what the main window shows. Input: the theme the main window
    /// shows now. Output: Dark for Dark, Light for Light, and the shown theme for System.
    pub fn icon(self, shown: Theme) -> Theme {
        match self {
            Self::System if shown == Theme::Dark => Theme::Dark,
            Self::System | Self::Light => Theme::Light,
            Self::Dark => Theme::Dark,
        }
    }
}

/// What the native side last showed: the owner's choice and the icon painted for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mirror {
    pub choice: ThemeChoice,
    pub icon: Theme,
}

/// The app's one mirror of the webview's theme choice; empty until the webview first mirrors.
#[derive(Default)]
pub struct Mirrored(Mutex<Option<Mirror>>);

/// Reads what the native side last showed. Input: the app. Output: the mirror, or none before
/// the webview first mirrored its choice.
pub fn mirrored<R: Runtime>(app: &AppHandle<R>) -> Option<Mirror> {
    let state = app.try_state::<Mirrored>()?;
    *state.0.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Holds every window to the choice, then paints the icon for what the main window shows.
/// The window and Dock calls run on the main thread, where AppKit takes them, and the main
/// window's theme is read after its own set, so System reads the live appearance.
/// Inputs: the app and the choice. Output: an error when the work could not be queued.
pub fn apply<R: Runtime>(app: &AppHandle<R>, choice: ThemeChoice) -> tauri::Result<()> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        for window in handle.webview_windows().into_values() {
            let _ = window.set_theme(choice.held());
        }
        let shown = handle
            .get_webview_window("main")
            .and_then(|main| main.theme().ok())
            .unwrap_or(Theme::Light);
        show(&handle, choice, shown);
    })
}

/// Repaints the icon when the system appearance changed and the choice follows it.
/// Inputs: the app and the theme the main window shows now. Output: none.
pub fn followed<R: Runtime>(app: &AppHandle<R>, shown: Theme) {
    let follows = mirrored(app).map(|mirror| mirror.choice) == Some(ThemeChoice::System);
    if follows {
        show(app, ThemeChoice::System, shown);
    }
}

/// Gives a window opened after the first mirror the theme the others are held to and, on
/// Windows, the icon they show. Input: the new window. Output: none.
pub fn dress<R: Runtime>(window: &WebviewWindow<R>) {
    let Some(mirror) = mirrored(window.app_handle()) else {
        return;
    };
    let _ = window.set_theme(mirror.choice.held());
    #[cfg(windows)]
    let _ = window.set_icon(window_picture(mirror.icon));
}

/// Keeps the choice and paints the icon that matches it. Inputs: the app, the choice and the
/// theme the main window shows. Output: none.
fn show<R: Runtime>(app: &AppHandle<R>, choice: ThemeChoice, shown: Theme) {
    let mirror = Mirror {
        choice,
        icon: choice.icon(shown),
    };
    if let Some(state) = app.try_state::<Mirrored>() {
        *state.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(mirror);
    }
    paint(app, mirror.icon);
}

/// Shows the icon in the Dock of the running app: the tile drawn by an image view in the
/// Dock tile, scaled into the tile. Off the main thread nothing changes. Inputs: the app and
/// the icon theme. Output: none.
#[cfg(target_os = "macos")]
fn paint<R: Runtime>(_app: &AppHandle<R>, icon: Theme) {
    use objc2::rc::Retained;
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage, NSImageView, NSView};
    use objc2_foundation::{NSData, NSPoint, NSRect};

    let Some(marker) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(dock_picture(icon));
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    let tile = NSApplication::sharedApplication(marker).dockTile();
    let view: Retained<NSImageView> = NSImageView::imageViewWithImage(&image, marker);
    view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), tile.size()));
    let content: &NSView = &view;
    tile.setContentView(Some(content));
    tile.display();
}

/// Shows the icon on every window and its taskbar button. Tao sets the small window icon and
/// leaves the large one unset, so the taskbar draws the small one. Inputs: the app and the
/// icon theme. Output: none.
#[cfg(windows)]
fn paint<R: Runtime>(app: &AppHandle<R>, icon: Theme) {
    for window in app.webview_windows().into_values() {
        let _ = window.set_icon(window_picture(icon));
    }
}

/// Linux keeps the one fixed icon (DEC-181 addendum 1). Inputs: unused. Output: none.
#[cfg(not(any(target_os = "macos", windows)))]
fn paint<R: Runtime>(_app: &AppHandle<R>, _icon: Theme) {}

/// The Dock picture for an icon theme: the 1024 px tile. Input: the theme. Output: the PNG.
#[cfg(any(target_os = "macos", test))]
fn dock_picture(icon: Theme) -> &'static [u8] {
    if icon == Theme::Dark {
        include_bytes!("../icons/icon.png")
    } else {
        include_bytes!("../icons/light/icon.png")
    }
}

/// The window and taskbar picture for an icon theme: the 256 px tile, decoded when built.
/// Input: the theme. Output: the RGBA image.
#[cfg(any(windows, test))]
fn window_picture(icon: Theme) -> Image<'static> {
    if icon == Theme::Dark {
        tauri::include_image!("icons/128x128@2x.png")
    } else {
        tauri::include_image!("icons/light/128x128@2x.png")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
    use tauri::{App, WebviewUrl, WebviewWindowBuilder};

    fn app_with_main() -> App<MockRuntime> {
        let app = mock_builder()
            .manage(Mirrored::default())
            .build(mock_context(noop_assets()))
            .unwrap();
        WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
            .build()
            .unwrap();
        app
    }

    fn tile_top(picture: &Image<'_>) -> [u8; 4] {
        let at = ((40 * picture.width() + picture.width() / 2) * 4) as usize;
        picture.rgba()[at..at + 4].try_into().unwrap()
    }

    fn png_width(png: &[u8]) -> u32 {
        assert_eq!(&png[1..4], b"PNG");
        u32::from_be_bytes(png[16..20].try_into().unwrap())
    }

    #[test]
    fn the_icon_follows_the_choice_and_system_follows_what_the_window_shows() {
        assert_eq!(ThemeChoice::System.held(), None);
        assert_eq!(ThemeChoice::Light.held(), Some(Theme::Light));
        assert_eq!(ThemeChoice::Dark.held(), Some(Theme::Dark));
        for (choice, shown, icon) in [
            (ThemeChoice::System, Theme::Dark, Theme::Dark),
            (ThemeChoice::System, Theme::Light, Theme::Light),
            (ThemeChoice::Light, Theme::Dark, Theme::Light),
            (ThemeChoice::Light, Theme::Light, Theme::Light),
            (ThemeChoice::Dark, Theme::Light, Theme::Dark),
            (ThemeChoice::Dark, Theme::Dark, Theme::Dark),
        ] {
            assert_eq!(choice.icon(shown), icon, "{choice:?} on {shown:?}");
        }
    }

    #[test]
    fn the_choice_reads_only_the_three_words_the_webview_keeps() {
        for (word, choice) in [
            ("system", ThemeChoice::System),
            ("light", ThemeChoice::Light),
            ("dark", ThemeChoice::Dark),
        ] {
            let read: ThemeChoice = serde_json::from_value(serde_json::json!(word)).unwrap();
            assert_eq!(read, choice, "{word}");
        }
        for word in [
            serde_json::json!("Dark"),
            serde_json::json!("blue"),
            serde_json::json!(""),
            serde_json::json!(1),
            serde_json::json!(null),
        ] {
            assert!(
                serde_json::from_value::<ThemeChoice>(word.clone()).is_err(),
                "{word}"
            );
        }
    }

    #[test]
    fn a_mirrored_choice_paints_the_icon_for_what_the_main_window_shows() {
        let app = app_with_main();
        let handle = app.handle();
        assert_eq!(mirrored(handle), None);
        followed(handle, Theme::Dark);
        assert_eq!(mirrored(handle), None, "no choice to follow yet");
        apply(handle, ThemeChoice::Dark).unwrap();
        let dark = Mirror {
            choice: ThemeChoice::Dark,
            icon: Theme::Dark,
        };
        assert_eq!(mirrored(handle), Some(dark));
        followed(handle, Theme::Light);
        assert_eq!(mirrored(handle), Some(dark), "held Dark");
        apply(handle, ThemeChoice::System).unwrap();
        let system = |icon| Mirror {
            choice: ThemeChoice::System,
            icon,
        };
        assert_eq!(
            mirrored(handle),
            Some(system(Theme::Light)),
            "the mock main window shows light"
        );
        followed(handle, Theme::Dark);
        assert_eq!(mirrored(handle), Some(system(Theme::Dark)));
        followed(handle, Theme::Light);
        assert_eq!(mirrored(handle), Some(system(Theme::Light)));
        apply(handle, ThemeChoice::Light).unwrap();
        followed(handle, Theme::Dark);
        let light = Mirror {
            choice: ThemeChoice::Light,
            icon: Theme::Light,
        };
        assert_eq!(mirrored(handle), Some(light));
    }

    #[test]
    fn the_dock_and_window_pictures_are_the_light_and_dark_tiles() {
        let light = window_picture(Theme::Light);
        let dark = window_picture(Theme::Dark);
        assert_eq!((light.width(), light.height()), (256, 256));
        assert_eq!((dark.width(), dark.height()), (256, 256));
        let (top_light, top_dark) = (tile_top(&light), tile_top(&dark));
        assert!(top_light.iter().all(|&value| value > 200), "{top_light:?}");
        assert!(
            top_dark[..3].iter().all(|&value| value < 64),
            "{top_dark:?}"
        );
        assert_eq!(top_dark[3], 255, "{top_dark:?}");
        assert_eq!((light.rgba()[3], dark.rgba()[3]), (0, 0), "clear corners");
        assert_eq!(png_width(dock_picture(Theme::Light)), 1024);
        assert_eq!(png_width(dock_picture(Theme::Dark)), 1024);
        assert_ne!(dock_picture(Theme::Light), dock_picture(Theme::Dark));
        assert_eq!(
            dock_picture(Theme::Dark),
            include_bytes!("../icons/icon.png"),
            "the fixed default icon is the dark night tile"
        );
    }
}
