#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod notifications;
mod support;
use emc_core::{
    diagnostics::{DiagnosticLog, DiagnosticStatus},
    Engine, Settings, Status,
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};

struct Desktop {
    engine: Result<Engine, String>,
    export_lock: tokio::sync::Mutex<()>,
    firewall_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    status_notices: std::sync::Mutex<notifications::StatusNotifications>,
}
impl Desktop {
    fn engine(&self) -> Result<Engine, String> {
        self.engine.clone()
    }
}
type CommandResult<T> = Result<T, String>;

#[tauri::command]
fn open_project_url() -> CommandResult<()> {
    support::open_project()
}

#[tauri::command]
async fn add_peer_firewall_rule(
    state: State<'_, Desktop>,
    address: String,
) -> CommandResult<String> {
    let peer = support::peer_ip(&address)?;
    let guard = state
        .firewall_lock
        .clone()
        .try_lock_owned()
        .map_err(|_| "The Windows firewall helper is already running.".to_string())?;
    tokio::task::spawn_blocking(move || {
        // Ownership follows the OS helper if the WebView request is cancelled.
        let _guard = guard;
        support::add_firewall_rule(peer)
    })
    .await
    .map_err(|_| "The Windows firewall helper could not complete.".to_string())?
}

// A small local facade keeps all policy in the Rust engine.
#[tauri::command]
fn get_status(state: State<'_, Desktop>) -> CommandResult<Status> {
    Ok(state.engine()?.status())
}
#[tauri::command]
fn get_diagnostic_status(state: State<'_, Desktop>) -> CommandResult<DiagnosticStatus> {
    if let Ok(engine) = state.engine() {
        return Ok(engine.diagnostic_status());
    }
    let directory = Engine::diagnostic_directory()
        .map_err(|_| "The local diagnostic directory is unavailable.".to_string())?;
    DiagnosticLog::existing_status(&directory)
        .map_err(|_| "Unable to inspect the local diagnostic log files.".to_string())
}
#[tauri::command]
fn update_settings(state: State<'_, Desktop>, settings: Settings) -> CommandResult<Status> {
    state
        .engine()?
        .update_settings(settings)
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn create_invitation(state: State<'_, Desktop>) -> CommandResult<String> {
    state
        .engine()?
        .create_invitation()
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn copy_invitation(state: State<'_, Desktop>) -> CommandResult<()> {
    let engine = state.engine()?;
    tokio::task::spawn_blocking(move || engine.copy_invitation())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn pair_device(
    state: State<'_, Desktop>,
    address: String,
    invitation: String,
) -> CommandResult<()> {
    state
        .engine()?
        .pair_device(address, invitation)
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn forget_peer(state: State<'_, Desktop>) -> CommandResult<()> {
    state
        .engine()?
        .forget_peer()
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn reconnect(state: State<'_, Desktop>) -> CommandResult<()> {
    state
        .engine()?
        .reconnect()
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn emergency_stop(state: State<'_, Desktop>) -> CommandResult<()> {
    state
        .engine()?
        .emergency_stop()
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn send_file(state: State<'_, Desktop>, path: String) -> CommandResult<()> {
    state
        .engine()?
        .send_file(path)
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn respond_file(state: State<'_, Desktop>, id: String, accept: bool) -> CommandResult<()> {
    state
        .engine()?
        .respond_file(id, accept)
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn diagnostics(state: State<'_, Desktop>, address: String) -> CommandResult<String> {
    state
        .engine()?
        .diagnostics(address)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn export_diagnostics(state: State<'_, Desktop>) -> CommandResult<String> {
    let _guard = state
        .export_lock
        .try_lock()
        .map_err(|_| "A diagnostic export is already in progress.".to_string())?;
    let exported = match state.engine() {
        Ok(engine) => engine.export_diagnostics().await,
        Err(_) => {
            let directory = Engine::diagnostic_directory()
                .map_err(|_| "The local diagnostic directory is unavailable.".to_string())?;
            DiagnosticLog::export_existing(directory).await
        }
    };
    let path = exported.map_err(|_| {
        "Unable to export diagnostics. Check available disk space and try again.".to_string()
    })?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
fn open_log_folder(state: State<'_, Desktop>) -> CommandResult<()> {
    let directory = match state.engine() {
        Ok(engine) => std::path::PathBuf::from(engine.diagnostic_status().directory),
        Err(_) => Engine::diagnostic_directory()
            .map_err(|_| "The local diagnostic directory is unavailable.".to_string())?,
    };
    open_known_directory(directory)
}

#[tauri::command]
fn open_export_folder() -> CommandResult<()> {
    let logs = Engine::diagnostic_directory()
        .map_err(|_| "The local diagnostic directory is unavailable.".to_string())?;
    let directory = logs
        .parent()
        .ok_or_else(|| "The local export directory is unavailable.".to_string())?
        .join("diagnostic-exports");
    if !directory.is_dir() {
        return Err("Export a diagnostic ZIP before opening its folder.".into());
    }
    open_known_directory(directory)
}

// These callers supply only fixed application directories. No command accepts
// a frontend path, executable, URL or additional arguments.
fn open_known_directory(directory: std::path::PathBuf) -> CommandResult<()> {
    #[cfg(target_os = "windows")]
    {
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| "The Windows system directory is unavailable.".to_string())?;
        // Only the engine-owned diagnostic directory can be opened. The WebView
        // cannot supply an executable, argument, URL, or arbitrary filesystem path.
        std::process::Command::new(std::path::PathBuf::from(system_root).join("explorer.exe"))
            .arg(directory)
            .spawn()
            .map_err(|_| "Unable to open the local diagnostic folder.".to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = directory;
        Err("Opening diagnostic folders is available in the Windows desktop app.".into())
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn tray_error(app: &AppHandle, message: &str) {
    show_main_window(app);
    let _ = app.emit("app://action-error", message);
}

fn create_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Easy Mouse Connect", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause sharing", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop sharing now", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Easy Mouse Connect", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause, &stop, &separator, &quit])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))?;
    TrayIconBuilder::with_id("workspace")
        .icon(icon)
        .menu(&menu)
        .tooltip("Easy Mouse Connect — running in the system tray")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "pause" => {
                if let Ok(engine) = app.state::<Desktop>().engine() {
                    let mut settings = engine.status().settings;
                    settings.enabled = !settings.enabled;
                    if engine.update_settings(settings).is_err() {
                        tray_error(app, "Sharing could not be changed. Check Diagnostics.");
                    }
                }
            }
            "stop" => {
                if let Ok(engine) = app.state::<Desktop>().engine() {
                    if engine.emergency_stop().is_err() {
                        tray_error(
                            app,
                            "The sharing setting could not be saved. Check Diagnostics.",
                        );
                    }
                }
            }
            "quit" => {
                if let Ok(engine) = app.state::<Desktop>().engine() {
                    engine.shutdown();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => show_main_window(tray.app_handle()),
            _ => {}
        })
        .build(app)?;

    let handle = app.handle().clone();
    // Keep tray actions accurate when settings change in the window or the local
    // emergency shortcut pauses sharing while the window is hidden.
    tauri::async_runtime::spawn(async move {
        let mut previous = None;
        loop {
            let engine = handle.state::<Desktop>().engine().ok();
            if let Some(engine) = &engine {
                handle
                    .state::<Desktop>()
                    .status_notices
                    .lock()
                    .unwrap()
                    .poll(&engine.notification_snapshot());
                if engine.is_shutting_down() {
                    break;
                }
            }
            let status = engine.map(|e| e.status());
            let enabled = status.as_ref().map(|s| s.enabled);
            let recovery = status.as_ref().is_some_and(|s| {
                s.enabled
                    && s.input_diagnostics.held_input_recovery_required
                    && !s.input_diagnostics.sending
                    && !s.input_diagnostics.receiving
            });
            if previous != Some((enabled, recovery)) {
                let _ = pause.set_enabled(enabled.is_some());
                let _ = stop.set_enabled(enabled == Some(true));
                let _ = pause.set_text(if enabled == Some(false) {
                    "Resume sharing"
                } else {
                    "Pause sharing"
                });
                if let Some(tray) = handle.tray_by_id("workspace") {
                    let _ = tray.set_tooltip(Some(match (enabled, recovery) {
                        (Some(true), true) => {
                            "Easy Mouse Connect — release held input locally; open for help"
                        }
                        (Some(true), false) => "Easy Mouse Connect — running; click to open",
                        (Some(false), _) => "Easy Mouse Connect — sharing paused; click to open",
                        (None, _) => "Easy Mouse Connect — engine unavailable; click to open",
                    }));
                }
                previous = Some((enabled, recovery));
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    Ok(())
}

fn main() {
    let app = tauri::Builder::default()
        // The official native plugin runs before engine setup. A second launch
        // focuses this process and cannot start another listener/input engine.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .setup(|app| {
            let engine =
                tauri::async_runtime::block_on(Engine::start()).map_err(|e| format!("{e:#}"));
            #[cfg(windows)]
            let notification_window = app
                .get_webview_window("main")
                .and_then(|window| window.hwnd().ok())
                .map_or(0, |window| window.0 as usize);
            #[cfg(not(windows))]
            let notification_window = 0;
            app.manage(Desktop {
                engine,
                export_lock: tokio::sync::Mutex::new(()),
                firewall_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
                status_notices: std::sync::Mutex::new(notifications::StatusNotifications::new(
                    notification_window,
                )),
            });
            create_tray(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_diagnostic_status,
            update_settings,
            create_invitation,
            copy_invitation,
            pair_device,
            forget_peer,
            reconnect,
            emergency_stop,
            send_file,
            respond_file,
            diagnostics,
            export_diagnostics,
            open_log_folder,
            open_export_folder,
            open_project_url,
            add_peer_firewall_rule
        ])
        .build(tauri::generate_context!())
        .expect("Unable to start Easy Mouse Connect");
    #[cfg(windows)]
    let app = {
        let mut app = app;
        // Raw input registration is process-wide. Stop Tao's optional device
        // events before run/Ready invokes setup and starts our native backend.
        // Calling this later would remove the backend's registration as well.
        app.set_device_event_filter(tauri::DeviceEventFilter::Always);
        app
    };
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            app.state::<Desktop>()
                .status_notices
                .lock()
                .unwrap()
                .clear();
            if let Ok(engine) = app.state::<Desktop>().engine() {
                engine.shutdown();
            }
        }
    });
}
