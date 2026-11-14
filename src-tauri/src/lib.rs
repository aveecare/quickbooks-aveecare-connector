//! AveeCare QuickBooks Connector.
//!
//! Replaces Intuit's Web Connector for AveeCare agencies on QuickBooks
//! Desktop: the office signs in with their AveeCare login, and the app carries
//! AveeCare's qbXML to QuickBooks through Intuit's own COM interface and the
//! answers back. All payroll and billing logic stays on the server.

pub mod api;
pub mod auth;
pub mod aveecare;
pub mod config;
pub mod qb;
pub mod session;
pub mod srp;
pub mod store;
pub mod sync;

use api::Profile;
use serde::Serialize;
use serde_json::{json, Value};
use session::{KeyringStore, Session};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use store::{Settings, Store};
use sync::{run_sync, RunRecord, RunStatus, SyncContext, SyncEvent};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_opener::OpenerExt as _;
use tauri_plugin_updater::UpdaterExt as _;

pub struct AppState {
    store: Store,
    settings: Mutex<Settings>,
    session: Mutex<Arc<Session>>,
    profile: Mutex<Option<Profile>>,
    running: AtomicBool,
    /// An update is downloading or installing: no sync may start until it ends.
    updating: AtomicBool,
    cancel: Arc<AtomicBool>,
    last_auto: Mutex<Option<Instant>>,
}

impl AppState {
    fn session(&self) -> Arc<Session> {
        self.session.lock().unwrap().clone()
    }
}

/// An error the window can show: a stable code and a sentence.
#[derive(Debug, Serialize)]
pub struct UiError {
    code: String,
    message: String,
}

fn ui_err(code: &str, message: impl Into<String>) -> UiError {
    UiError { code: code.into(), message: message.into() }
}

impl From<auth::AuthError> for UiError {
    fn from(e: auth::AuthError) -> Self {
        let v = serde_json::to_value(&e).unwrap_or(Value::Null);
        ui_err(v.get("code").and_then(|c| c.as_str()).unwrap_or("Other"), e.to_string())
    }
}

impl From<api::ApiError> for UiError {
    fn from(e: api::ApiError) -> Self {
        match &e {
            api::ApiError::SignedOut(m) => ui_err("SignedOut", m.clone()),
            api::ApiError::Refused { reason, message } => ui_err(reason, message.clone()),
            api::ApiError::Network(m) => ui_err("Network", m.clone()),
            api::ApiError::Server(m) => ui_err("Server", m.clone()),
        }
    }
}

/// The QuickBooks emulator stands in for QuickBooks only when this is set to
/// a loopback address (testing; see README).
fn emulator_url() -> Option<String> {
    std::env::var(config::EMULATOR_ENV).ok().and_then(|u| qb::emulator::loopback_url(&u))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuickBooksStatus {
    /// "ready", "notRunning", "elevationMismatch", "emulator", "unknown"
    state: String,
    title: String,
    fix: String,
    self_elevated: bool,
}

fn quickbooks_status_now() -> QuickBooksStatus {
    if let Some(url) = emulator_url() {
        return if qb::emulator::answering(&url) {
            QuickBooksStatus { state: "emulator".into(), title: "Using the QuickBooks emulator (testing).".into(), fix: String::new(), self_elevated: false }
        } else {
            QuickBooksStatus {
                state: "notRunning".into(),
                title: "The QuickBooks emulator is not answering.".into(),
                fix: "Start the QuickBooks emulator.".into(),
                self_elevated: false,
            }
        };
    }
    #[cfg(windows)]
    {
        let me = qb::process::self_elevated();
        let (running, elevated) = match qb::process::quickbooks_process() {
            qb::process::QbProcess::NotRunning => (false, None),
            qb::process::QbProcess::Running { elevated } => (true, elevated),
        };
        match sync::preflight(running, elevated, me) {
            None => QuickBooksStatus { state: "ready".into(), title: "QuickBooks is open.".into(), fix: String::new(), self_elevated: me },
            Some(e) => QuickBooksStatus {
                state: serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
                title: e.title,
                fix: e.fix,
                self_elevated: me,
            },
        }
    }
    #[cfg(not(windows))]
    QuickBooksStatus { state: "unknown".into(), title: "QuickBooks Desktop runs on Windows only.".into(), fix: String::new(), self_elevated: false }
}

#[tauri::command]
fn app_info(state: State<AppState>) -> Value {
    json!({
        "version": config::APP_VERSION,
        "serverUrl": state.session().server_url(),
        "dataDir": state.store.dir().display().to_string(),
        "emulator": emulator_url(),
    })
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Value {
    let settings = state.settings.lock().unwrap().clone();
    let session = state.session();
    json!({
        "signedIn": session.has_saved_sign_in(),
        "email": session.email(),
        "profile": state.profile.lock().unwrap().clone(),
        "settings": settings,
        "history": state.store.history(),
        "running": state.running.load(Ordering::SeqCst),
        "quickbooks": quickbooks_status_now(),
    })
}

#[tauri::command]
fn quickbooks_status() -> QuickBooksStatus {
    quickbooks_status_now()
}

#[tauri::command]
async fn sign_in(state: State<'_, AppState>, email: String, password: String) -> Result<Value, UiError> {
    let session = state.session();
    let step = tauri::async_runtime::spawn_blocking(move || session.sign_in(&email, &password)).await.map_err(|e| ui_err("Other", e.to_string()))??;
    match step {
        auth::SignInStep::Done(_) => Ok(json!({ "done": true })),
        auth::SignInStep::TotpCode { session, username } => Ok(json!({ "needsCode": true, "session": session, "username": username })),
    }
}

#[tauri::command]
async fn confirm_code(state: State<'_, AppState>, session: String, username: String, code: String) -> Result<(), UiError> {
    let s = state.session();
    tauri::async_runtime::spawn_blocking(move || s.confirm_code(&session, &username, &code)).await.map_err(|e| ui_err("Other", e.to_string()))??;
    Ok(())
}

/// After sign-in and at launch: who is this, and may they connect. The server
/// decides; a refusal signs the app out with the server's sentence.
#[tauri::command]
async fn load_profile(app: AppHandle, state: State<'_, AppState>) -> Result<Profile, UiError> {
    let s = state.session();
    let result = tauri::async_runtime::spawn_blocking(move || s.profile()).await.map_err(|e| ui_err("Other", e.to_string()))?;
    match result {
        Ok(p) => {
            *state.profile.lock().unwrap() = Some(p.clone());
            if let Some(email) = state.session().email() {
                let mut st = state.settings.lock().unwrap();
                st.last_email = email;
                let _ = state.store.save_settings(&st);
            }
            refresh_tray(&app, &state);
            Ok(p)
        }
        Err(e) => {
            if matches!(e, api::ApiError::SignedOut(_) | api::ApiError::Refused { .. }) {
                *state.profile.lock().unwrap() = None;
                if matches!(e, api::ApiError::Refused { .. }) {
                    // Not allowed to connect: do not keep a sign-in that cannot be used.
                    state.session().sign_out();
                }
            }
            Err(e.into())
        }
    }
}

#[tauri::command]
async fn sign_out(app: AppHandle, state: State<'_, AppState>) -> Result<(), UiError> {
    if state.running.load(Ordering::SeqCst) {
        return Err(ui_err("Busy", "A sync is running. Wait for it to finish, then sign out."));
    }
    let s = state.session();
    let _ = tauri::async_runtime::spawn_blocking(move || s.sign_out()).await;
    *state.profile.lock().unwrap() = None;
    refresh_tray(&app, &state);
    Ok(())
}

/// Start a sync. Progress arrives as `sync-event`; the record as `sync-finished`.
#[tauri::command]
fn sync_now(app: AppHandle, state: State<AppState>, allow_file: Option<String>) -> Result<(), UiError> {
    start_sync(&app, &state, "manual", allow_file)
}

#[tauri::command]
fn cancel_sync(state: State<AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

fn start_sync(app: &AppHandle, state: &AppState, trigger: &str, allow_file: Option<String>) -> Result<(), UiError> {
    if state.running.swap(true, Ordering::SeqCst) {
        return Err(ui_err("Busy", "A sync is already running."));
    }
    // Claimed `running` first, then looked at `updating` (install_update does
    // the reverse), so a sync and an update can never both go ahead.
    if state.updating.load(Ordering::SeqCst) {
        state.running.store(false, Ordering::SeqCst);
        return Err(ui_err("Busy", "The Connector is updating. Sync again once it reopens."));
    }
    state.cancel.store(false, Ordering::SeqCst);
    let app = app.clone();
    let trigger = trigger.to_string();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let record = sync_on_this_thread(&app, &state, &trigger, allow_file);
        let _ = state.store.add_run(&record);
        if matches!(record.status, RunStatus::Completed | RunStatus::CompletedWithProblems | RunStatus::NothingToSend) {
            // Remember this file as the agency's, for the next run's check.
            let company = state.profile.lock().unwrap().as_ref().map(|p| p.company_name.clone()).unwrap_or_default();
            let mut st = state.settings.lock().unwrap();
            st.remember_file(&company, &record.company_file);
            let _ = state.store.save_settings(&st);
        }
        if record.problem.as_ref().map(|p| p.code == "SIGNED_OUT").unwrap_or(false) {
            *state.profile.lock().unwrap() = None;
        }
        state.running.store(false, Ordering::SeqCst);
        let _ = app.emit("sync-finished", &record);
        refresh_tray(&app, &state);
    });
    Ok(())
}

fn sync_on_this_thread(app: &AppHandle, state: &AppState, trigger: &str, allow_file: Option<String>) -> RunRecord {
    let session = state.session();
    let api = session.api();
    let token = || session.id_token();
    let company = state.profile.lock().unwrap().as_ref().map(|p| p.company_name.clone()).unwrap_or_default();
    let expected = state.settings.lock().unwrap().known_file(&company).map(String::from);
    let ctx = SyncContext {
        api: &api,
        token: &token,
        cancel: &state.cancel,
        trigger,
        expected_file: expected.as_deref(),
        allow_file: allow_file.as_deref(),
    };
    let mut emit = |e: SyncEvent| {
        let _ = app.emit("sync-event", &e);
    };

    if let Some(url) = emulator_url() {
        return match qb::emulator::EmulatorTransport::new(&url) {
            Ok(mut t) => run_sync(&ctx, &mut t, &mut emit),
            Err(e) => failed_record(trigger, e),
        };
    }
    #[cfg(windows)]
    {
        let status = quickbooks_status_now();
        if status.state != "ready" {
            let kind = if status.state == "elevationMismatch" { qb::QbErrorKind::ElevationMismatch } else { qb::QbErrorKind::NotRunning };
            return failed_record(trigger, qb::QbError::new(kind, &status.title, &status.fix));
        }
        let mut t = qb::com::ComTransport::new();
        run_sync(&ctx, &mut t, &mut emit)
    }
    #[cfg(not(windows))]
    failed_record(trigger, qb::QbError::new(qb::QbErrorKind::NotInstalled, "QuickBooks Desktop runs on Windows only.", ""))
}

fn failed_record(trigger: &str, e: qb::QbError) -> RunRecord {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    RunRecord {
        id: uuid::Uuid::new_v4().to_string(),
        started_at: now.clone(),
        finished_at: now,
        trigger: trigger.into(),
        status: RunStatus::Failed,
        mode: String::new(),
        plan: Default::default(),
        company_file: String::new(),
        product_name: String::new(),
        qbxml_version: String::new(),
        steps: vec![],
        totals: Default::default(),
        problem: Some(sync::RunProblem {
            source: "quickbooks".into(),
            code: serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
            title: e.title,
            fix: e.fix,
            raw: e.raw,
        }),
        closing: String::new(),
        requests: 0,
    }
}

#[tauri::command]
fn clear_history(state: State<AppState>) -> Result<(), UiError> {
    state.store.clear_history().map_err(|e| ui_err("Other", e.to_string()))
}

#[tauri::command]
fn save_settings(app: AppHandle, state: State<AppState>, auto_sync_minutes: u32, start_with_windows: bool) -> Result<Settings, UiError> {
    let allowed = [0, 15, 30, 60, 120, 240];
    let minutes = if allowed.contains(&auto_sync_minutes) { auto_sync_minutes } else { 0 };
    let autolaunch = app.autolaunch();
    let result = if start_with_windows { autolaunch.enable() } else { autolaunch.disable() };
    if let Err(e) = result {
        return Err(ui_err("Other", format!("Windows did not accept the start-up setting: {e}")));
    }
    // Read back what Windows now has, rather than trusting the write.
    let actual = autolaunch.is_enabled().unwrap_or(start_with_windows);
    let mut st = state.settings.lock().unwrap();
    st.auto_sync_minutes = minutes;
    st.start_with_windows = actual;
    state.store.save_settings(&st).map_err(|e| ui_err("Other", e.to_string()))?;
    *state.last_auto.lock().unwrap() = Some(Instant::now());
    Ok(st.clone())
}

/// Testing and support only: point the app at another AveeCare server.
/// Signs out, because a sign-in belongs to one server.
#[tauri::command]
fn set_server(app: AppHandle, state: State<AppState>, server_url: String) -> Result<Settings, UiError> {
    let url = server_url.trim().trim_end_matches('/').to_string();
    let parsed = url::Url::parse(&url).map_err(|_| ui_err("Other", "That is not a web address."))?;
    let local = matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1"));
    if parsed.scheme() != "https" && !local {
        return Err(ui_err("Other", "Use an https:// address."));
    }
    if state.running.load(Ordering::SeqCst) {
        return Err(ui_err("Busy", "Wait for the sync to finish."));
    }
    state.session().sign_out();
    *state.profile.lock().unwrap() = None;
    *state.session.lock().unwrap() = Arc::new(Session::new(&url, Box::new(KeyringStore)));
    let mut st = state.settings.lock().unwrap();
    st.server_url = url;
    state.store.save_settings(&st).map_err(|e| ui_err("Other", e.to_string()))?;
    refresh_tray(&app, &state);
    Ok(st.clone())
}

/// Open a page of AveeCare in the browser (a fix link from a refused item).
#[tauri::command]
fn open_in_aveecare(app: AppHandle, state: State<AppState>, path: String) -> Result<(), UiError> {
    let path = if path.starts_with('/') { path } else { format!("/{path}") };
    if path.starts_with("//") {
        return Err(ui_err("Other", "Not an AveeCare page."));
    }
    let url = format!("{}{}", state.session().server_url(), path);
    app.opener().open_url(url, None::<&str>).map_err(|e| ui_err("Other", e.to_string()))
}

/// QuickBooks is running as administrator: run the Connector the same way.
#[tauri::command]
fn restart_elevated(app: AppHandle) -> Result<(), UiError> {
    #[cfg(windows)]
    {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let exe = std::env::current_exe().map_err(|e| ui_err("Other", e.to_string()))?;
        let file = HSTRING::from(exe.as_os_str());
        let verb = HSTRING::from("runas");
        let r = unsafe { ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
        // ShellExecute returns a value above 32 on success.
        if r.0 as isize <= 32 {
            return Err(ui_err("Other", "Windows did not restart the Connector as administrator."));
        }
        app.exit(0);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err(ui_err("Other", "Windows only."))
    }
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Value, UiError> {
    let updater = app.updater().map_err(|e| ui_err("Other", e.to_string()))?;
    match updater.check().await {
        Ok(Some(u)) => Ok(json!({ "available": true, "version": u.version, "notes": u.body })),
        Ok(None) => Ok(json!({ "available": false })),
        Err(e) => Err(ui_err("Network", format!("Could not check for updates: {e}"))),
    }
}

/// Run one AveeCare payroll or billing operation with this sign-in (the
/// operations Payroll and Billing in the web app call; see aveecare.rs).
#[tauri::command]
async fn aveecare_call(state: State<'_, AppState>, api: String, operation: String, input: Value) -> Result<Value, UiError> {
    let which = aveecare::Api::parse(&api).ok_or_else(|| ui_err("Other", "Unknown AveeCare area."))?;
    let s = state.session();
    let result = tauri::async_runtime::spawn_blocking(move || s.aveecare(which, &operation, &input))
        .await
        .map_err(|e| ui_err("Other", e.to_string()))?;
    result.map_err(UiError::from)
}

/// The longest a whole update download may take (the installer is a few MB).
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// No bytes for this long and the download has stopped.
const UPDATE_STALL: Duration = Duration::from_secs(90);

#[tauri::command]
async fn install_update(app: AppHandle, state: State<'_, AppState>) -> Result<(), UiError> {
    if state.updating.swap(true, Ordering::SeqCst) {
        return Err(ui_err("Busy", "The update is already installing."));
    }
    // Held for the whole download and install; released only if it fails, so
    // an automatic sync can never start partway and be cut off by the installer.
    struct Release<'a>(&'a AtomicBool);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let release = Release(&state.updating);
    if state.running.load(Ordering::SeqCst) {
        return Err(ui_err("Busy", "Wait for the sync to finish, then update."));
    }
    let updater = app.updater().map_err(|e| ui_err("Other", e.to_string()))?;
    let mut update = updater.check().await.map_err(|e| ui_err("Network", e.to_string()))?.ok_or_else(|| ui_err("Other", "You already have the newest version."))?;
    // A stalled download must end in an error the window can offer Try again
    // and "Open this version" for, never a progress bar that waits forever.
    // The plugin does not carry the builder's timeout into the download (its
    // Update starts with none), so the limit is set on the Update itself, and a
    // watchdog stops a download that has sent nothing for UPDATE_STALL.
    update.timeout = Some(UPDATE_DOWNLOAD_TIMEOUT);
    let started = Instant::now();
    let last_chunk_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let downloaded_all = Arc::new(AtomicBool::new(false));
    // Progress for the window: bytes so far and the total when GitHub sends one.
    let progress_app = app.clone();
    let mut downloaded: u64 = 0;
    let chunk_seen = last_chunk_ms.clone();
    let finished = downloaded_all.clone();
    let install = update.download_and_install(
        move |chunk, total| {
            downloaded += chunk as u64;
            chunk_seen.store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
            let _ = progress_app.emit("update-progress", json!({ "downloaded": downloaded, "total": total }));
        },
        move || finished.store(true, Ordering::SeqCst),
    );
    // Watches the download only: once every byte is in, the installer runs
    // undisturbed (dropping it partway through installing is never done).
    let watchdog = async {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if downloaded_all.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let idle = started.elapsed().saturating_sub(Duration::from_millis(last_chunk_ms.load(Ordering::SeqCst)));
            if idle >= UPDATE_STALL {
                return;
            }
        }
    };
    tokio::select! {
        r = install => r.map_err(|e| ui_err("Other", format!("The update did not install: {e}")))?,
        _ = watchdog => return Err(ui_err("Network", "The download stopped partway. Check the internet connection, then press Try again.")),
    }
    // Installed: keep `updating` held until the app closes.
    std::mem::forget(release);
    // On Windows the installer closes the app and opens the new version itself;
    // this restart covers the case where it did not.
    app.restart();
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn refresh_tray(app: &AppHandle, state: &AppState) {
    if let Some(tray) = app.tray_by_id("main") {
        let running = state.running.load(Ordering::SeqCst);
        let last = state.store.history().into_iter().next();
        let tip = if running {
            "AveeCare Connector: syncing…".to_string()
        } else if state.profile.lock().unwrap().is_none() {
            "AveeCare Connector: signed out".to_string()
        } else {
            match last.map(|r| r.status) {
                Some(RunStatus::Completed) | Some(RunStatus::NothingToSend) => "AveeCare Connector: up to date".into(),
                Some(RunStatus::CompletedWithProblems) => "AveeCare Connector: some items need attention".into(),
                Some(RunStatus::Stopped) | Some(RunStatus::Failed) => "AveeCare Connector: last sync did not finish".into(),
                _ => "AveeCare Connector".into(),
            }
        };
        let _ = tray.set_tooltip(Some(tip));
    }
}

/// Automatic syncs on the chosen interval. Quiet when QuickBooks is not open:
/// an automatic run that could not start adds nothing to the activity list.
fn auto_sync_loop(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(20));
        let state = app.state::<AppState>();
        let minutes = state.settings.lock().unwrap().auto_sync_minutes;
        if minutes == 0 || state.running.load(Ordering::SeqCst) || state.profile.lock().unwrap().is_none() {
            continue;
        }
        let due = state.last_auto.lock().unwrap().map(|t| t.elapsed() >= Duration::from_secs(minutes as u64 * 60)).unwrap_or(true);
        if !due {
            continue;
        }
        *state.last_auto.lock().unwrap() = Some(Instant::now());
        let qb = quickbooks_status_now();
        if qb.state != "ready" && qb.state != "emulator" {
            continue;
        }
        let _ = start_sync(&app, &state, "auto", None);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = Store::new(Store::default_dir());
    let settings = store.settings();
    let session = Arc::new(Session::new(&settings.server_url, Box::new(KeyringStore)));
    let start_hidden = std::env::args().any(|a| a == "--minimized");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            store,
            settings: Mutex::new(settings),
            session: Mutex::new(session),
            profile: Mutex::new(None),
            running: AtomicBool::new(false),
            updating: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            last_auto: Mutex::new(Some(Instant::now())),
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_state,
            quickbooks_status,
            sign_in,
            confirm_code,
            load_profile,
            sign_out,
            aveecare_call,
            sync_now,
            cancel_sync,
            clear_history,
            save_settings,
            set_server,
            open_in_aveecare,
            restart_elevated,
            check_update,
            install_update
        ])
        .setup(move |app| {
            let open = MenuItem::with_id(app, "open", "Open AveeCare Connector", true, None::<&str>)?;
            let sync = MenuItem::with_id(app, "sync", "Sync now", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&open, &sync, &sep, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .tooltip("AveeCare Connector")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "sync" => {
                        show_main(app);
                        let state = app.state::<AppState>();
                        if state.profile.lock().unwrap().is_some() {
                            let _ = start_sync(app, &state, "manual", None);
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // Keep the start-up setting honest: Windows may have changed it.
            let state = app.state::<AppState>();
            if let Ok(enabled) = app.autolaunch().is_enabled() {
                let mut st = state.settings.lock().unwrap();
                if st.start_with_windows != enabled {
                    st.start_with_windows = enabled;
                    let _ = state.store.save_settings(&st);
                }
            }
            if !start_hidden {
                show_main(app.handle());
            }
            auto_sync_loop(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the Connector in the tray, like the Web Connector.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running the AveeCare QuickBooks Connector");
}
