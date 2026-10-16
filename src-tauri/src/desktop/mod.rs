//! Desktop tray lifecycle: closing the main window keeps the gateway running.
//! The tray preference is cached atomically so the OS close event never blocks
//! on SQLite. The explicit "Exit" action always terminates the process.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, Runtime,
};

pub const TRAY_ID: &str = "freeapibox-tray";

pub struct TrayPreference {
    enabled: AtomicBool,
}

impl TrayPreference {
    pub fn new(enabled: bool) -> Self {
        Self { enabled: AtomicBool::new(enabled) }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
}

fn show_main<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window.show()
            .and_then(|()| window.unminimize())
            .and_then(|()| window.set_focus())
        {
            tracing::warn!(%error, "failed to restore FreeApiBox main window");
        }
    }
}

/// The native tray is created regardless of the saved setting, so changing
/// the setting at runtime does not require restarting the application.
pub fn setup_tray<R: Runtime>(app: &mut tauri::App<R>, enabled: bool) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "tray-show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "退出 FreeApiBox", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("FreeApiBox")
        // Left click restores the window; right click opens the native menu.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-show" => show_main(app),
            "tray-quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(event, TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }) {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;
    tray.set_visible(enabled)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_tray_preference_is_mutable_without_relaunch() {
        let state = TrayPreference::new(true);
        assert!(state.enabled());
        state.set_enabled(false);
        assert!(!state.enabled());
        state.set_enabled(true);
        assert!(state.enabled());
    }
}
