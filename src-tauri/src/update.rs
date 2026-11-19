// SPDX-License-Identifier: GPL-3.0-or-later
//! Signed desktop self-updates.
//!
//! The updater plugin verifies the artifact against the public key in
//! `tauri.conf.json` before it returns any bytes, and verifies again before
//! installation. This module owns the policy around it:
//!
//! * a check or install is refused in private and read-only mode;
//! * automatic checks run at most once a day and never download anything;
//! * installing waits while a download, map transfer or archive move is
//!   writing into the profile, because installation restarts the process;
//! * only one update operation runs at a time;
//! * the last check is recorded in `update-state.json` in the profile.
//!
//! The page never invokes the plugin's JavaScript commands: the capabilities
//! file grants no updater permission. `update_status`, `update_check` and
//! `update_install` travel through the normal command dispatch, so they inherit
//! the same privacy lease and window ownership checks as every other command.

use base64::Engine;
use crate::service::Service;
use crate::store::Settings;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};
use url::Url;

pub(crate) const CHANNEL: &str = "stable";
const CHECK_INTERVAL_SECONDS: i64 = 24 * 60 * 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const STATE_FILE: &str = "update-state.json";
const PROGRESS_EVENT: &str = "kiwix-update-progress";
const AVAILABLE_EVENT: &str = "kiwix-update-available";
const NOT_CONFIGURED: &str =
    "This build was not set up for updates. Ask whoever supplied Kiwix for an updated build.";
const SIGNATURE_REFUSED: &str =
    "Kiwix could not confirm that the update came from Kiwix, so it kept the version you have. Download the update again from the official Kiwix site.";
const CHECK_AGAIN: &str = "Check for updates again before installing.";
const BUSY: &str = "An update is already being checked or installed. Wait for it to finish.";

// ---------------------------------------------------------------------------
// Platform capability
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    // Production constructs only its native target; capability tests retain
    // the complete cross-platform matrix.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Macos,
    /// The App Sandbox cannot replace the app bundle it runs from.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MacosSandboxed,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Windows,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Linux,
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    Ios,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    Android,
    // Capability fallback for targets outside the five native integrations.
    #[allow(dead_code)]
    Other,
}

pub(crate) fn current_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        if crate::sandbox::is_sandboxed() {
            Platform::MacosSandboxed
        } else {
            Platform::Macos
        }
    }
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(target_os = "linux")]
    {
        Platform::Linux
    }
    #[cfg(target_os = "ios")]
    {
        Platform::Ios
    }
    #[cfg(target_os = "android")]
    {
        Platform::Android
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux",
        target_os = "ios",
        target_os = "android"
    )))]
    {
        Platform::Other
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Capability {
    pub platform: &'static str,
    pub self_update: bool,
    /// `self`, `app-store`, `play-store` or `manual`.
    pub management: &'static str,
    pub store_name: Option<&'static str>,
    pub explanation: String,
}

pub(crate) fn capability_for(platform: Platform) -> Capability {
    match platform {
        Platform::Macos | Platform::Windows | Platform::Linux => Capability {
            platform: match platform {
                Platform::Macos => "macos",
                Platform::Windows => "windows",
                _ => "linux",
            },
            self_update: true,
            management: "self",
            store_name: None,
            explanation:
                "Kiwix can download a signed update from the official update channel. Installing one closes and reopens the app."
                    .into(),
        },
        Platform::MacosSandboxed => Capability {
            platform: "macos",
            self_update: false,
            management: "manual",
            store_name: None,
            explanation:
                "This Mac app runs in the macOS App Sandbox, which does not let an app replace itself. Download the latest version from the official Kiwix site to update."
                    .into(),
        },
        Platform::Ios => Capability {
            platform: "ios",
            self_update: false,
            management: "app-store",
            store_name: Some("App Store"),
            explanation:
                "iPhone and iPad apps update through the App Store. This build cannot update itself. Open the App Store to install the latest version."
                    .into(),
        },
        Platform::Android => Capability {
            platform: "android",
            self_update: false,
            management: "play-store",
            store_name: Some("Google Play"),
            explanation:
                "Android apps update through Google Play or the installer that supplied Kiwix. This build cannot update itself."
                    .into(),
        },
        Platform::Other => Capability {
            platform: "other",
            self_update: false,
            management: "manual",
            store_name: None,
            explanation:
                "This build cannot update itself. Install a newer version from whoever supplied Kiwix."
                    .into(),
        },
    }
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// A privileged update action needs a writable, non-private profile. Both
/// modes are explained instead of silently hiding the action.
pub(crate) fn mode_blocker(settings: &Settings) -> Option<String> {
    if settings.private_mode {
        Some(
            "Updates are unavailable while Kiwix is in private mode, because checking contacts the network and installing restarts the app. Leave private mode, then check again."
                .into(),
        )
    } else if settings.read_only {
        Some(
            "Updates are unavailable while Kiwix is read-only, because installing changes the app. Leave read-only mode, then check again."
                .into(),
        )
    } else {
        None
    }
}

/// Automatic checks obey the same metered-network allowance as downloads.
/// They never download an artifact; installation is always an explicit action.
pub(crate) fn automatic_check_allowed(settings: &Settings, metered: bool) -> bool {
    !metered || settings.allow_metered_downloads
}

/// Statuses that own a write into the profile. Terminal, paused and seeding
/// rows hold no in-progress write, so an install can proceed.
pub(crate) fn transfer_in_progress(status: &str) -> bool {
    !matches!(
        status,
        "complete"
            | "completed"
            | "failed"
            | "error"
            | "cancelled"
            | "paused"
            | "seeding"
    )
}

// ---------------------------------------------------------------------------
// Single-operation gate
// ---------------------------------------------------------------------------

pub(crate) struct OperationGate(AtomicBool);

impl OperationGate {
    pub(crate) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    pub(crate) fn try_begin(&self) -> Option<OperationGuard<'_>> {
        self.0
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| OperationGuard(&self.0))
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub(crate) struct OperationGuard<'a>(&'a AtomicBool);

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

static OPERATION: OperationGate = OperationGate::new();

/// Set by a completed install that must relaunch. The relaunch is performed by
/// the ordinary Quit path only after every shell flushed and `Service::shutdown`
/// succeeded. A cancelled prepare clears it so a later plain Quit never restarts.
/// A retry of the same prepared retirement retains the requested relaunch.
static RESTART_AFTER_QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Consume the pending relaunch request (Quit retired the service).
pub(crate) fn take_restart_after_quit() -> bool {
    RESTART_AFTER_QUIT.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Preparation was cancelled and the shells resumed. The next Quit is a plain exit.
pub(crate) fn clear_restart_after_quit() {
    RESTART_AFTER_QUIT.store(false, std::sync::atomic::Ordering::SeqCst);
}
static PENDING: Mutex<Option<Update>> = Mutex::new(None);
static PROGRESS: Mutex<Option<Progress>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Persisted check state
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredState {
    pub last_checked: Option<String>,
    pub last_available_version: Option<String>,
    pub last_error: Option<String>,
}

pub(crate) fn load_state_at(path: &Path) -> StoredState {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn save_state_at(path: &Path, state: &StoredState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        // Windows refuses to replace an existing destination.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
            std::fs::rename(&temporary, path).map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn save_state(store: &crate::store::Store, state: &StoredState) {
    if store.private_mode().unwrap_or(true) || store.read_only().unwrap_or(true) {
        return;
    }
    let path = store.data_dir().join(STATE_FILE);
    if let Err(error) = save_state_at(&path, state) {
        eprintln!("KIWIX_UPDATE_STATE {error}");
    }
}

pub(crate) fn check_due(last_checked: Option<&str>, now: time::OffsetDateTime) -> bool {
    let Some(last_checked) = last_checked else {
        return true;
    };
    match crate::timestamps::parse(last_checked) {
        Ok(checked) => (now - checked).whole_seconds()
            >= CHECK_INTERVAL_SECONDS,
        Err(_) => true,
    }
}

fn next_check_at(last_checked: Option<&str>) -> Option<String> {
    let checked = crate::timestamps::parse(last_checked?).ok()?;
    checked.checked_add(time::Duration::seconds(CHECK_INTERVAL_SECONDS)).map(crate::timestamps::format)
}

// ---------------------------------------------------------------------------
// Manifest/channel configuration
// ---------------------------------------------------------------------------

struct ChannelConfig {
    endpoint: Url,
    pubkey: String,
}

/// The public key and endpoint live in `tauri.conf.json`. The environment
/// override exists for the documented loopback acceptance run; a shipped
/// build never sets it.
fn channel_config(app: &AppHandle) -> Result<ChannelConfig, String> {
    let plugin = app.config().plugins.0.get("updater");
    // Runtime trust-root overrides are only available to debug acceptance builds.
    // A release must use the key compiled into its signed application.
    let endpoint = acceptance_override("KIWIX_UPDATER_ENDPOINT")
        .or_else(|| {
            plugin
                .and_then(|config| config.get("endpoints"))
                .and_then(Value::as_array)
                .and_then(|endpoints| endpoints.first())
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| NOT_CONFIGURED.to_string())?;
    let pubkey = acceptance_override("KIWIX_UPDATER_PUBKEY")
        .or_else(|| {
            plugin
                .and_then(|config| config.get("pubkey"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| NOT_CONFIGURED.to_string())?;
    if pubkey.trim().is_empty() {
        return Err(NOT_CONFIGURED.into());
    }
    if base64::engine::general_purpose::STANDARD
        .decode(pubkey.trim())
        .is_err()
    {
        return Err(
            "The update key in this build is not valid. Ask whoever supplied Kiwix for an updated build."
                .into(),
        );
    }
    let endpoint = Url::parse(&endpoint).map_err(|_| {
        "The update address in this build is not valid. Ask whoever supplied Kiwix for an updated build."
            .to_string()
    })?;
    Ok(ChannelConfig { endpoint, pubkey })
}

fn acceptance_override(name: &str) -> Option<String> {
    #[cfg(debug_assertions)]
    {
        std::env::var(name).ok()
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = name;
        None
    }
}

fn updater_for(app: &AppHandle, config: &ChannelConfig) -> Result<tauri_plugin_updater::Updater, String> {
    app.updater_builder()
        .endpoints(vec![config.endpoint.clone()])
        .map_err(|error| plain_update_error(&error))?
        .pubkey(config.pubkey.clone())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| plain_update_error(&error))
}

fn app_handle(service: &Arc<Service>) -> Result<AppHandle, String> {
    service
        .app
        .lock()
        .map_err(|_| "Application handle unavailable")?
        .clone()
        .ok_or_else(|| "Updates are not available until Kiwix finishes starting. Try again in a moment.".into())
}

fn plain_update_error(error: &UpdaterError) -> String {
    match error {
        UpdaterError::Minisign(_)
        | UpdaterError::SignatureUtf8(_)
        | UpdaterError::Base64(_)
        | UpdaterError::SignedVersionMismatch { .. }
        | UpdaterError::MissingSignedVersion => SIGNATURE_REFUSED.into(),
        UpdaterError::EmptyEndpoints | UpdaterError::InsecureTransportProtocol => {
            "This build has no usable update address. Ask whoever supplied Kiwix for an updated build."
                .into()
        }
        UpdaterError::ReleaseNotFound => {
            "No update information was found at the update address. Try again later.".into()
        }
        UpdaterError::Network(_) | UpdaterError::Reqwest(_) => {
            "Kiwix could not reach the update service. Check your connection, then try again."
                .into()
        }
        UpdaterError::TargetNotFound(_)
        | UpdaterError::TargetsNotFound(_)
        | UpdaterError::UnsupportedOs
        | UpdaterError::UnsupportedArch => {
            "This version of Kiwix does not have an update for this computer. Download the latest version from the official Kiwix site."
                .into()
        }
        UpdaterError::AuthenticationFailed
        | UpdaterError::DebInstallFailed
        | UpdaterError::PackageInstallFailed
        | UpdaterError::TempDirNotOnSameMountPoint
        | UpdaterError::PreviousAppNotRestored(_)
        | UpdaterError::BinaryNotFoundInArchive
        | UpdaterError::TempDirNotFound
        | UpdaterError::InvalidUpdaterFormat => {
            "Kiwix could not install the update, so it kept the version you have. Install the new version from the official Kiwix site."
                .into()
        }
        _ => {
            "The update could not be completed, so Kiwix kept the version you have. Try again, or download the latest version from the official Kiwix site."
                .into()
        }
    }
}

// ---------------------------------------------------------------------------
// Status and summaries
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSummary {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub phase: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateStatus {
    pub platform: &'static str,
    pub supported: bool,
    pub management: &'static str,
    pub store_name: Option<&'static str>,
    pub explanation: String,
    pub channel: &'static str,
    pub current_version: String,
    pub configured: bool,
    pub configuration_note: Option<String>,
    pub blocked_reason: Option<String>,
    pub last_checked: Option<String>,
    /// False where Kiwix never checks on its own (the sandboxed macOS build).
    pub automatic_checks: bool,
    pub next_check_at: Option<String>,
    pub last_available_version: Option<String>,
    pub last_error: Option<String>,
    pub busy: bool,
    pub available: Option<UpdateSummary>,
    pub install_blocked_reason: Option<String>,
    pub progress: Option<Progress>,
}

fn summary(update: &Update) -> UpdateSummary {
    UpdateSummary {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
        date: update.date.as_ref().map(|date| date.to_string()),
    }
}

fn set_progress(app: &AppHandle, progress: Option<Progress>) {
    if let Ok(mut current) = PROGRESS.lock() {
        *current = progress.clone();
    }
    let _ = app.emit(PROGRESS_EVENT, progress);
}

pub(crate) async fn status(service: &Arc<Service>) -> Result<UpdateStatus, String> {
    let capability = capability_for(current_platform());
    let settings = service.store.settings()?;
    let app = service
        .app
        .lock()
        .map_err(|_| "Application handle unavailable")?
        .clone();
    let current_version = app
        .as_ref()
        .map(|app| app.package_info().version.to_string())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let (configured, configuration_note) = match app.as_ref() {
        Some(app) => match channel_config(app) {
            Ok(_) => (true, None),
            Err(error) => (false, Some(error)),
        },
        None => (
            false,
            Some("Updates are not available until Kiwix finishes starting. Try again in a moment.".into()),
        ),
    };
    let stored = load_state_at(&service.store.data_dir().join(STATE_FILE));
    let available = PENDING
        .lock()
        .map_err(|_| "Update state unavailable")?
        .as_ref()
        .map(summary);
    let install_blocked_reason = if capability.self_update {
        match service.update_install_blocker().await {
            Ok(reason) => reason,
            Err(_) => Some(
                "Kiwix could not check whether a transfer is in progress. Wait a moment, then try again."
                    .into(),
            ),
        }
    } else {
        None
    };
    Ok(UpdateStatus {
        platform: capability.platform,
        supported: capability.self_update,
        management: capability.management,
        store_name: capability.store_name,
        explanation: capability.explanation,
        channel: CHANNEL,
        current_version,
        configured,
        configuration_note,
        blocked_reason: mode_blocker(&settings),
        automatic_checks: !crate::sandbox::is_sandboxed(),
        next_check_at: next_check_at(stored.last_checked.as_deref()),
        last_checked: stored.last_checked,
        last_available_version: stored.last_available_version,
        last_error: stored.last_error,
        busy: OPERATION.is_busy(),
        available,
        install_blocked_reason,
        progress: PROGRESS
            .lock()
            .map_err(|_| "Update progress unavailable")?
            .clone(),
    })
}

// ---------------------------------------------------------------------------
// Check
// ---------------------------------------------------------------------------

pub(crate) async fn check(
    service: &Arc<Service>,
    automatic: bool,
) -> Result<Option<UpdateSummary>, String> {
    let settings = service.store.settings()?;
    if let Some(reason) = mode_blocker(&settings) {
        return Err(reason);
    }
    let app = app_handle(service)?;
    let config = channel_config(&app)?;
    if automatic {
        // The same Internet admission used by downloads. Desktop has no
        // metered policy, but the helper keeps the mobile rule explicit.
        crate::transfer_policy::require_internet()?;
        if !automatic_check_allowed(&settings, false) {
            return Err(
                "Kiwix does not check for updates automatically on a metered connection. Connect to Wi-Fi or allow metered downloads in Settings."
                    .into(),
            );
        }
    }
    let _operation = OPERATION.try_begin().ok_or_else(|| BUSY.to_string())?;
    let updater = updater_for(&app, &config)?;
    let result = updater.check().await;
    let now = time::OffsetDateTime::now_utc();
    let path = service.store.data_dir().join(STATE_FILE);
    match result {
        Ok(update) => {
            let mut stored = load_state_at(&path);
            stored.last_checked = Some(crate::timestamps::format(now));
            stored.last_available_version = update.as_ref().map(|update| update.version.clone());
            stored.last_error = None;
            save_state(&service.store, &stored);
            let summary = update.as_ref().map(summary);
            *PENDING
                .lock()
                .map_err(|_| "Update state unavailable")? = update;
            if let Some(summary) = &summary {
                let _ = app.emit(AVAILABLE_EVENT, summary.clone());
            }
            Ok(summary)
        }
        Err(error) => {
            let plain = plain_update_error(&error);
            let mut stored = load_state_at(&path);
            stored.last_checked = Some(crate::timestamps::format(now));
            stored.last_error = Some(plain.clone());
            save_state(&service.store, &stored);
            Err(plain)
        }
    }
}

/// Background check: no download, no prompt, no error band. It runs after the
/// window is usable and only when the persisted last check is at least a day
/// old.
pub(crate) async fn automatic_check(service: Arc<Service>) {
    // The sandboxed macOS build cannot install updates and offers no manual
    // path either, so it does not contact the update server on its own.
    if crate::sandbox::is_sandboxed() || service.ensure_running().is_err() {
        return;
    }
    let Ok(settings) = service.store.settings() else {
        return;
    };
    if mode_blocker(&settings).is_some() {
        return;
    }
    let Ok(app) = app_handle(&service) else {
        return;
    };
    if channel_config(&app).is_err() {
        return;
    }
    let stored = load_state_at(&service.store.data_dir().join(STATE_FILE));
    if !check_due(stored.last_checked.as_deref(), time::OffsetDateTime::now_utc()) {
        return;
    }
    if !automatic_check_allowed(&settings, false) {
        return;
    }
    if let Err(error) = check(&service, true).await {
        eprintln!("KIWIX_UPDATE_CHECK {error}");
    }
}

pub(crate) fn start_daily_check(service: Arc<Service>) {
    tauri::async_runtime::spawn(async move {
        // Let startup, window restore and the first snapshot settle first.
        tokio::time::sleep(Duration::from_secs(10)).await;
        loop {
            if service.ensure_running().is_err() {
                break;
            }
            service.update_auto_check().await;
            tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

pub(crate) async fn install(
    service: &Arc<Service>,
    expected_version: Option<&str>,
    _installation: tokio::sync::OwnedRwLockWriteGuard<()>,
) -> Result<(), String> {
    let settings = service.store.settings()?;
    if let Some(reason) = mode_blocker(&settings) {
        return Err(reason);
    }
    let capability = capability_for(current_platform());
    if !capability.self_update {
        return Err(capability.explanation);
    }
    let app = app_handle(service)?;
    let _config = channel_config(&app)?;
    let _operation = OPERATION.try_begin().ok_or_else(|| BUSY.to_string())?;
    // Dispatch acquired the exclusive lease before privacy/transfer locks.
    // Keep it alive across both the snapshot and awaited artifact download.
    service.ensure_running()?;
    if let Some(reason) = service.update_install_blocker().await? {
        return Err(reason);
    }
    let update = {
        let pending = PENDING
            .lock()
            .map_err(|_| "Update state unavailable")?;
        let update = pending.as_ref().ok_or_else(|| CHECK_AGAIN.to_string())?;
        if let Some(expected) = expected_version {
            if expected != update.version {
                return Err("The available update changed. Check for updates again.".into());
            }
        }
        update.clone()
    };

    let mut downloaded = 0u64;
    let mut total: Option<u64> = None;
    let mut last_emit = Instant::now();
    set_progress(
        &app,
        Some(Progress {
            downloaded: 0,
            total: None,
            phase: "downloading".into(),
        }),
    );
    let progress_app = app.clone();
    let bytes = update
        .download(
            move |chunk, content_length| {
                downloaded = downloaded.saturating_add(chunk as u64);
                if content_length.is_some() {
                    total = content_length;
                }
                if last_emit.elapsed() >= Duration::from_millis(100) {
                    last_emit = Instant::now();
                    set_progress(
                        &progress_app,
                        Some(Progress {
                            downloaded,
                            total,
                            phase: "downloading".into(),
                        }),
                    );
                }
            },
            || {},
        )
        .await
        .map_err(|error| {
            set_progress(&app, None);
            plain_update_error(&error)
        })?;

    set_progress(
        &app,
        Some(Progress {
            downloaded,
            total: Some(total.unwrap_or(downloaded)),
            phase: "verifying".into(),
        }),
    );
    // `download` already verified these bytes; verify again at the install
    // boundary so an unsigned payload can never reach the installer.
    update.verify(&bytes).map_err(|error| {
        set_progress(&app, None);
        plain_update_error(&error)
    })?;
    set_progress(
        &app,
        Some(Progress {
            downloaded,
            total: Some(total.unwrap_or(downloaded)),
            phase: "installing".into(),
        }),
    );
    update.install(&bytes).map_err(|error| {
        set_progress(&app, None);
        plain_update_error(&error)
    })?;
    set_progress(&app, None);
    if let Ok(mut pending) = PENDING.lock() {
        *pending = None;
    }
    let path = service.store.data_dir().join(STATE_FILE);
    let mut stored = load_state_at(&path);
    stored.last_available_version = None;
    stored.last_error = None;
    save_state(&service.store, &stored);

    // Windows starts its installer and exits inside `install`. macOS and
    // Linux replace the bundle in place, so relaunch explicitly.
    //
    // Never `restart()`/`request_restart()` here: Tauri raises ExitRequested
    // with RESTART_EXIT_CODE, for which `prevent_exit` is ignored, so the
    // process would exit (and relaunch) while the Quit flush and
    // `Service::shutdown` were still running: unsaved sessions and unretired
    // transfers. Request an ordinary, preventable exit instead; the Quit path
    // relaunches only after it retired the service. Returning first also drops
    // this command's privacy and admission leases, which that shutdown needs.
    #[cfg(not(target_os = "windows"))]
    {
        RESTART_AFTER_QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
        app.exit(0);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) async fn command(
    service: Arc<Service>,
    command: &str,
    args: &Value,
    installation: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
) -> Result<Value, String> {
    match command {
        "update_status" => value(status(&service).await?),
        "update_check" => {
            check(&service, false).await?;
            value(status(&service).await?)
        }
        "update_install" => {
            if args.get("confirmed") != Some(&Value::Bool(true)) {
                return Err("Installing an update requires confirmation.".into());
            }
            let expected = args.get("version").and_then(Value::as_str);
            let installation = installation.ok_or("Update installation admission is unavailable")?;
            install(&service, expected, installation).await?;
            Ok(Value::Null)
        }
        _ => Err("Unknown update action".into()),
    }
}

fn value<T: Serialize>(input: T) -> Result<Value, String> {
    serde_json::to_value(input).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> Settings {
        Settings::default()
    }

    #[test]
    fn platform_capabilities_are_honest() {
        for platform in [Platform::Macos, Platform::Windows, Platform::Linux] {
            let capability = capability_for(platform);
            assert!(capability.self_update);
            assert_eq!(capability.management, "self");
        }
        let ios = capability_for(Platform::Ios);
        assert!(!ios.self_update);
        assert_eq!(ios.management, "app-store");
        assert_eq!(ios.store_name, Some("App Store"));
        let sandboxed = capability_for(Platform::MacosSandboxed);
        assert!(!sandboxed.self_update);
        assert_eq!(sandboxed.platform, "macos");
        assert_eq!(sandboxed.management, "manual");
        let android = capability_for(Platform::Android);
        assert!(!android.self_update);
        assert_eq!(android.management, "play-store");
        assert_eq!(android.store_name, Some("Google Play"));
    }

    #[test]
    fn private_and_read_only_modes_refuse_with_an_explanation() {
        let mut settings = test_settings();
        assert_eq!(mode_blocker(&settings), None);
        settings.private_mode = true;
        assert!(mode_blocker(&settings).unwrap().contains("private mode"));
        settings.private_mode = false;
        settings.read_only = true;
        assert!(mode_blocker(&settings).unwrap().contains("read-only"));
    }

    #[test]
    fn metered_connections_block_automatic_checks_without_an_allowance() {
        let mut settings = test_settings();
        settings.allow_metered_downloads = false;
        assert!(!automatic_check_allowed(&settings, true));
        assert!(automatic_check_allowed(&settings, false));
        settings.allow_metered_downloads = true;
        assert!(automatic_check_allowed(&settings, true));
    }

    #[test]
    fn concurrent_operations_are_refused() {
        let gate = OperationGate::new();
        let first = gate.try_begin().expect("first operation starts");
        assert!(gate.is_busy());
        assert!(gate.try_begin().is_none());
        drop(first);
        assert!(!gate.is_busy());
        assert!(gate.try_begin().is_some());
    }

    #[test]
    fn active_transfers_block_installing() {
        for status in [
            "queued",
            "downloading",
            "verifying",
            "checking",
            "creating",
            "metadata",
        ] {
            assert!(transfer_in_progress(status), "{status}");
        }
        for status in [
            "complete",
            "completed",
            "failed",
            "error",
            "cancelled",
            "paused",
            "seeding",
        ] {
            assert!(!transfer_in_progress(status), "{status}");
        }
    }

    #[test]
    fn last_checked_persists_and_a_new_day_is_due() {
        let directory = std::env::temp_dir().join(format!(
            "kiwix-update-state-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(STATE_FILE);
        let checked = time::OffsetDateTime::now_utc();
        let state = StoredState {
            last_checked: Some(crate::timestamps::format(checked)),
            last_available_version: Some("0.1.1".into()),
            last_error: None,
        };
        save_state_at(&path, &state).unwrap();
        let loaded = load_state_at(&path);
        assert_eq!(loaded.last_checked, state.last_checked);
        assert_eq!(loaded.last_available_version.as_deref(), Some("0.1.1"));
        assert!(!check_due(loaded.last_checked.as_deref(), checked));
        assert!(check_due(
            loaded.last_checked.as_deref(),
            checked + time::Duration::hours(25)
        ));
        assert!(check_due(None, checked));
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn an_installed_update_relaunches_only_through_a_completed_quit() {
        // install() arms the relaunch and requests an ordinary exit; Quit
        // consumes it only after retirement, and clears it when cancelled.
        clear_restart_after_quit();
        assert!(!take_restart_after_quit(), "a plain Quit never restarts");
        RESTART_AFTER_QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
        clear_restart_after_quit(); // the flush timed out and the shells resumed
        assert!(!take_restart_after_quit(), "a cancelled Quit leaves no restart behind");
        RESTART_AFTER_QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(take_restart_after_quit());
        assert!(!take_restart_after_quit(), "the relaunch is consumed exactly once");
    }

    /// A known minisign test vector: the public key and a signature over
    /// `b"test"`. The Tauri plugin accepts the `.pub`/`.sig` file contents
    /// base64-encoded, exactly as the release flow supplies them.
    const TEST_PUBLIC_KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\ntrusted comment: timestamp:1555779966\tfile:test\nQtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";

    fn mock_loopback_app(endpoints: &[&str]) -> tauri::App<tauri::test::MockRuntime> {
        for endpoint in endpoints {
            let url = Url::parse(endpoint).unwrap();
            assert_eq!(url.scheme(), "http");
            assert_eq!(url.host_str(), Some("127.0.0.1"));
        }
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".into(),
            json!({
                "pubkey": "test",
                "endpoints": endpoints,
                "dangerousInsecureTransportProtocol": true,
            }),
        );
        tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock app builds")
    }

    fn mock_updater(endpoint: &str) -> (tauri::App<tauri::test::MockRuntime>, tauri_plugin_updater::Updater) {
        let app = mock_loopback_app(&[endpoint]);
        let updater = app
            .handle()
            .updater_builder()
            .pubkey(base64::engine::general_purpose::STANDARD.encode(TEST_PUBLIC_KEY))
            .build()
            .unwrap();
        (app, updater)
    }

    fn test_manifest(version: &str, signature: &str, url: &str) -> Value {
        json!({
            "version": version,
            "notes": "Test release",
            "pub_date": "2026-01-01T00:00:00Z",
            "platforms": {
                "darwin-aarch64": {"signature": signature, "url": url},
                "darwin-x86_64": {"signature": signature, "url": url},
                "linux-x86_64": {"signature": signature, "url": url},
                "windows-x86_64": {"signature": signature, "url": url}
            }
        })
    }

    /// Serve one manifest response from a loopback listener. `check` uses this
    /// exactly as it would use the real HTTPS endpoint.
    fn serve_manifest(manifest: Value) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = manifest.to_string();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        format!("http://{address}/update.json")
    }

    #[test]
    fn signature_verification_rejects_a_tampered_payload() {
        let signature = base64::engine::general_purpose::STANDARD.encode(TEST_SIGNATURE);
        let endpoint = serve_manifest(test_manifest(
            "0.1.1",
            &signature,
            "https://example.invalid/Kiwix.app.tar.gz",
        ));
        let (_app, updater) = mock_updater(&endpoint);
        let update = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(updater.check())
            .expect("manifest parses")
            .expect("version is newer");
        update.verify(b"test").expect("known signature verifies");
        let error = update.verify(b"tampered").unwrap_err();
        assert!(matches!(error, UpdaterError::Minisign(_)));
        assert!(plain_update_error(&error).contains("kept the version you have"));
    }

    /// Full loopback proof, driven by `scripts/update-e2e.mjs`:
    /// check -> download with progress -> install into a temporary app bundle,
    /// plus a tampered manifest that must be refused.
    #[test]
    #[ignore = "requires KIWIX_UPDATE_E2E_* variables from scripts/update-e2e.mjs"]
    fn loopback_end_to_end() {
        let manifest = std::env::var("KIWIX_UPDATE_E2E_MANIFEST").expect("manifest URL");
        let tampered_manifest =
            std::env::var("KIWIX_UPDATE_E2E_TAMPERED_MANIFEST").expect("tampered manifest URL");
        let pubkey = std::env::var("KIWIX_UPDATE_E2E_PUBKEY").expect("public key");
        let workdir = PathBuf::from(std::env::var("KIWIX_UPDATE_E2E_WORKDIR").expect("workdir"));
        let expected_version =
            std::env::var("KIWIX_UPDATE_E2E_VERSION").expect("expected version");
        let target = std::env::var("KIWIX_UPDATE_E2E_TARGET")
            .unwrap_or_else(|_| "darwin-aarch64-app".into());

        let app = mock_loopback_app(&[&manifest, &tampered_manifest]);
        let handle = app.handle().clone();

        #[cfg(target_os = "linux")]
        let executable = workdir.join("installed/Kiwix.AppImage");
        #[cfg(not(target_os = "linux"))]
        let executable = workdir.join("installed/Kiwix.app/Contents/MacOS/Kiwix");
        assert!(executable.is_file(),
            "harness must copy the actual old application before installation");

        let updater = handle
            .updater_builder()
            .endpoints(vec![Url::parse(&manifest).unwrap()])
            .unwrap()
            .pubkey(pubkey.clone())
            .target(target.clone())
            .executable_path(executable.clone())
            .build()
            .unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let update = updater
                .check()
                .await
                .expect("check succeeds")
                .expect("an update is available");
            assert_eq!(update.version, expected_version);
            assert!(update.body.is_some());

            let mut chunks = 0usize;
            let mut downloaded = 0u64;
            let bytes = update
                .download(
                    |chunk, _total| {
                        chunks += 1;
                        downloaded += chunk as u64;
                    },
                    || {},
                )
                .await
                .expect("signed artifact downloads");
            assert!(chunks > 0, "progress callbacks fired");
            assert_eq!(downloaded, bytes.len() as u64);
            update.install(&bytes).expect("signed artifact installs");
            #[cfg(target_os = "linux")]
            {
                assert_eq!(std::fs::read(&executable).unwrap().as_slice(), bytes.as_slice());
                let version = std::process::Command::new(&executable)
                    .env("APPIMAGE_EXTRACT_AND_RUN", "1")
                    .arg("--version")
                    .output()
                    .expect("launch the installed image");
                assert!(version.status.success());
                assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), format!("Kiwix Rust {expected_version}"));
            }
            #[cfg(not(target_os = "linux"))]
            {
                let installed = std::fs::read(workdir.join("installed/Kiwix.app/Contents/Info.plist")).unwrap();
                assert!(String::from_utf8_lossy(&installed).contains(&expected_version),
                    "the installed bundle reports version {expected_version}");
            }

            let tampered = handle
                .updater_builder()
                .endpoints(vec![Url::parse(&tampered_manifest).unwrap()])
                .unwrap()
                .pubkey(pubkey)
                .target(target)
                .executable_path(executable.clone())
                .build()
                .unwrap()
                .check()
                .await
                .expect("tampered manifest is readable")
                .expect("tampered manifest still announces an update");
            let error = tampered
                .download(|_, _| {}, || {})
                .await
                .expect_err("tampered artifact must be refused");
            assert!(matches!(error, UpdaterError::Minisign(_)));
            assert!(plain_update_error(&error).contains("kept the version you have"));
        });

        let state_path = workdir.join(STATE_FILE);
        let now = time::OffsetDateTime::now_utc();
        save_state_at(
            &state_path,
            &StoredState {
                last_checked: Some(crate::timestamps::format(now)),
                last_available_version: Some(expected_version),
                last_error: None,
            },
        )
        .unwrap();
        let loaded = load_state_at(&state_path);
        assert!(loaded.last_checked.is_some());
        assert!(!check_due(loaded.last_checked.as_deref(), now));
    }
}
