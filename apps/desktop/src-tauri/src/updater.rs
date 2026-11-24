//! Desktop auto-updater commands backed by Tauri v2 `tauri-plugin-updater`.
//!
//! Public key resolution priority:
//! 1. Compile-time `AICAD_UPDATER_PUBKEY` (optional CI injection)
//! 2. `plugins.updater.pubkey` in `tauri.conf.json` (or `--config` release overlay)
//!
//! When no signing public key is embedded (such as local dev builds), update checks
//! return `status = "unconfigured"` cleanly without crashing startup or making
//! unverifiable network requests.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

pub const APP_UPDATER_PROGRESS_EVENT: &str = "app-updater://progress";
pub const DEFAULT_UPDATER_ENDPOINT: &str =
    "https://github.com/nicechencs/ai-native-cad/releases/latest/download/latest.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdaterInfo {
    pub current_version: String,
    pub configured: bool,
    pub endpoint: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateCheckResult {
    /// One of: `"available"`, `"up_to_date"`, `"unconfigured"`, `"error"`.
    pub status: String,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    pub configured: bool,
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallAppUpdateRequest {
    #[serde(default = "default_true")]
    pub restart_after_install: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInstallResult {
    /// One of: `"installed"`, `"up_to_date"`, `"unconfigured"`.
    pub status: String,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    pub restarted: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdaterProgressEvent {
    /// One of: `"started"`, `"progress"`, `"finished"`.
    pub event: String,
    pub downloaded_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

pub(crate) fn compile_time_pubkey() -> Option<&'static str> {
    option_env!("AICAD_UPDATER_PUBKEY")
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

pub(crate) fn extract_configured_pubkey(
    plugin_config: Option<&serde_json::Value>,
    compile_override: Option<&str>,
) -> Option<String> {
    if let Some(key) = compile_override.map(str::trim).filter(|k| !k.is_empty()) {
        return Some(key.to_string());
    }
    plugin_config
        .and_then(|cfg| cfg.get("pubkey"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

pub(crate) fn extract_configured_endpoint(plugin_config: Option<&serde_json::Value>) -> String {
    plugin_config
        .and_then(|cfg| cfg.get("endpoints"))
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.first())
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_UPDATER_ENDPOINT.to_string())
}

pub(crate) fn build_updater_info(
    current_version: &str,
    plugin_config: Option<&serde_json::Value>,
    compile_override: Option<&str>,
) -> AppUpdaterInfo {
    let configured = extract_configured_pubkey(plugin_config, compile_override).is_some();
    let endpoint = extract_configured_endpoint(plugin_config);
    let message = if configured {
        format!("当前版本 v{current_version}，已启用签名自动更新。")
    } else {
        format!("当前版本 v{current_version}（未配置更新签名公钥，请使用官方签名安装包启用自动更新）。")
    };
    AppUpdaterInfo {
        current_version: current_version.to_string(),
        configured,
        endpoint,
        message,
    }
}

pub(crate) fn unconfigured_check_result(current_version: &str) -> AppUpdateCheckResult {
    AppUpdateCheckResult {
        status: "unconfigured".to_string(),
        current_version: current_version.to_string(),
        available_version: None,
        release_date: None,
        release_notes: None,
        download_url: None,
        configured: false,
        message: "当前构建未嵌入更新签名公钥（开发版或未签名构建），无法验证远程更新包。".to_string(),
    }
}

pub fn build_updater_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R, tauri_plugin_updater::Config> {
    let mut builder = tauri_plugin_updater::Builder::new();
    if let Some(pubkey) = compile_time_pubkey() {
        builder = builder.pubkey(pubkey);
    }
    builder.build()
}

fn updater_plugin_config(app: &AppHandle) -> Option<&serde_json::Value> {
    app.config().plugins.0.get("updater")
}

#[tauri::command]
pub fn get_app_updater_info(app: AppHandle) -> AppUpdaterInfo {
    let version = app.package_info().version.to_string();
    build_updater_info(&version, updater_plugin_config(&app), compile_time_pubkey())
}

#[tauri::command]
pub async fn check_app_update(app: AppHandle) -> Result<AppUpdateCheckResult, String> {
    let current_version = app.package_info().version.to_string();
    let configured =
        extract_configured_pubkey(updater_plugin_config(&app), compile_time_pubkey()).is_some();
    if !configured {
        return Ok(unconfigured_check_result(&current_version));
    }

    let updater = app
        .updater()
        .map_err(|err| format!("初始化自动更新器失败: {err}"))?;
    match updater
        .check()
        .await
        .map_err(|err| format!("检查更新失败: {err}"))?
    {
        Some(update) => {
            let release_date = update.date.map(|dt| dt.to_string());
            let available_version = update.version.clone();
            let message = format!("发现新版本 v{available_version}（当前 v{current_version}）。");
            Ok(AppUpdateCheckResult {
                status: "available".to_string(),
                current_version,
                available_version: Some(available_version),
                release_date,
                release_notes: update.body.clone(),
                download_url: Some(update.download_url.to_string()),
                configured: true,
                message,
            })
        }
        None => Ok(AppUpdateCheckResult {
            status: "up_to_date".to_string(),
            current_version: current_version.clone(),
            available_version: None,
            release_date: None,
            release_notes: None,
            download_url: None,
            configured: true,
            message: format!("当前 v{current_version} 已是最新版本。"),
        }),
    }
}

#[tauri::command]
pub async fn install_app_update(
    app: AppHandle,
    request: Option<InstallAppUpdateRequest>,
) -> Result<AppUpdateInstallResult, String> {
    let restart_after_install = request.unwrap_or_default().restart_after_install;
    let current_version = app.package_info().version.to_string();
    let configured =
        extract_configured_pubkey(updater_plugin_config(&app), compile_time_pubkey()).is_some();
    if !configured {
        return Ok(AppUpdateInstallResult {
            status: "unconfigured".to_string(),
            current_version,
            installed_version: None,
            restarted: false,
            message: "当前构建未嵌入更新签名公钥，无法安装自动更新。".to_string(),
        });
    }

    let updater = app
        .updater()
        .map_err(|err| format!("初始化自动更新器失败: {err}"))?;
    let Some(update) = updater
        .check()
        .await
        .map_err(|err| format!("检查更新失败: {err}"))?
    else {
        return Ok(AppUpdateInstallResult {
            status: "up_to_date".to_string(),
            current_version: current_version.clone(),
            installed_version: None,
            restarted: false,
            message: format!("当前 v{current_version} 已是最新版本，无需安装。"),
        });
    };

    let installed_version = update.version.clone();
    let _ = app.emit(
        APP_UPDATER_PROGRESS_EVENT,
        AppUpdaterProgressEvent {
            event: "started".to_string(),
            downloaded_bytes: 0,
            total_bytes: None,
        },
    );

    let mut downloaded: u64 = 0;
    let progress_app = app.clone();
    let finish_app = app.clone();
    update
        .download_and_install(
            move |chunk_length, content_length| {
                downloaded = downloaded.saturating_add(chunk_length as u64);
                let _ = progress_app.emit(
                    APP_UPDATER_PROGRESS_EVENT,
                    AppUpdaterProgressEvent {
                        event: "progress".to_string(),
                        downloaded_bytes: downloaded,
                        total_bytes: content_length,
                    },
                );
            },
            move || {
                let _ = finish_app.emit(
                    APP_UPDATER_PROGRESS_EVENT,
                    AppUpdaterProgressEvent {
                        event: "finished".to_string(),
                        downloaded_bytes: 0,
                        total_bytes: None,
                    },
                );
            },
        )
        .await
        .map_err(|err| format!("下载或安装更新失败: {err}"))?;

    if restart_after_install {
        // On Windows NSIS installer exits & relaunches automatically; on macOS/Linux request a graceful restart.
        #[cfg(not(windows))]
        app.request_restart();
    }

    Ok(AppUpdateInstallResult {
        status: "installed".to_string(),
        current_version,
        installed_version: Some(installed_version.clone()),
        restarted: restart_after_install,
        message: format!("已安装 v{installed_version}，应用将重启以生效。"),
    })
}
