//! Per-user runtime log directory.
//!
//! Same rule as `cad_agent.observe.resolve_log_dir`: `AICAD_LOG_DIR`, else
//! `<data>/aicad/logs`. Windows `<data>` is `%LOCALAPPDATA%`, then `%APPDATA%`.

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogPlatform {
    Windows,
    Macos,
    Unix,
}

#[derive(Debug, Clone)]
pub struct LogDirInputs {
    pub log_dir: Option<OsString>,
    pub local_app_data: Option<OsString>,
    pub app_data: Option<OsString>,
    pub home: Option<OsString>,
    pub xdg_data_home: Option<OsString>,
    pub temp_dir: PathBuf,
    pub platform: LogPlatform,
}

fn nonempty(value: &Option<OsString>) -> Option<&OsString> {
    value.as_ref().filter(|item| !item.is_empty())
}

/// Resolve the log directory without reading the process environment.
pub fn resolve_log_dir(inputs: &LogDirInputs) -> PathBuf {
    if let Some(dir) = nonempty(&inputs.log_dir) {
        return PathBuf::from(dir);
    }
    let base = match inputs.platform {
        LogPlatform::Windows => nonempty(&inputs.local_app_data)
            .or_else(|| nonempty(&inputs.app_data))
            .map(PathBuf::from),
        LogPlatform::Macos => nonempty(&inputs.home).map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        }),
        LogPlatform::Unix => nonempty(&inputs.xdg_data_home)
            .map(PathBuf::from)
            .or_else(|| {
                nonempty(&inputs.home).map(|home| PathBuf::from(home).join(".local").join("share"))
            }),
    };
    base.unwrap_or_else(|| inputs.temp_dir.clone())
        .join("aicad")
        .join("logs")
}

/// Application-data `logs/` directory: `AICAD_LOG_DIR`, else the per-user data dir.
pub fn default_log_dir() -> PathBuf {
    let var = |key: &str| std::env::var_os(key);
    let platform = if cfg!(windows) {
        LogPlatform::Windows
    } else if cfg!(target_os = "macos") {
        LogPlatform::Macos
    } else {
        LogPlatform::Unix
    };
    resolve_log_dir(&LogDirInputs {
        log_dir: var(crate::env::LOG_DIR),
        local_app_data: var("LOCALAPPDATA"),
        app_data: var("APPDATA"),
        home: var("HOME"),
        xdg_data_home: var("XDG_DATA_HOME"),
        temp_dir: std::env::temp_dir(),
        platform,
    })
}
