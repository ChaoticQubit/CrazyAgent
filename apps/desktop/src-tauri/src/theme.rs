use std::sync::{Mutex, PoisonError};

use serde::Deserialize;
use tauri::image::Image;
use tauri::{AppHandle, Manager, Runtime, Theme};

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
        None
    }

    /// The icon that matches what the main window shows. Input: the theme the main window
    /// shows now. Output: Dark for Dark, Light for Light, and the shown theme for System.
    pub fn icon(self, _shown: Theme) -> Theme {
        Theme::Light
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
/// Inputs: the app and the choice. Output: an error when the work could not be queued.
pub fn apply<R: Runtime>(_app: &AppHandle<R>, _choice: ThemeChoice) -> tauri::Result<()> {
    Ok(())
}

/// Repaints the icon when the system appearance changed and the choice follows it.
/// Inputs: the app and the theme the main window shows now. Output: none.
pub fn followed<R: Runtime>(_app: &AppHandle<R>, _shown: Theme) {}

/// The Dock picture for an icon theme: the 1024 px tile. Input: the theme. Output: the PNG.
pub fn dock_picture(_icon: Theme) -> &'static [u8] {
    include_bytes!("../icons/icon.png")
}

/// The window and taskbar picture for an icon theme: the 256 px tile, decoded when built.
/// Input: the theme. Output: the RGBA image.
pub fn window_picture(_icon: Theme) -> Image<'static> {
    tauri::include_image!("icons/128x128@2x.png")
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
        assert!(top_dark[..3].iter().all(|&value| value < 64), "{top_dark:?}");
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
