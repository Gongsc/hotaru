use parking_lot::{Mutex, RwLock};
use tauri::AppHandle;
use tokio::sync::watch;

use crate::models::{MonitorSnapshot, Settings};

/// Shared application state: settings, latest monitor snapshot and a
/// config epoch used to wake the monitor engine after settings changes.
pub struct AppState {
    pub settings: RwLock<Settings>,
    pub snapshot: RwLock<MonitorSnapshot>,
    pub config_epoch_tx: watch::Sender<u64>,
    /// Backend URL the "main" webview window is currently showing.
    pub loaded_panel_url: Mutex<Option<String>>,
}

impl AppState {
    pub fn bump_config_epoch(&self) {
        let next = *self.config_epoch_tx.borrow() + 1;
        let _ = self.config_epoch_tx.send(next);
    }
}

pub fn init(app: &AppHandle) -> AppState {
    let settings = crate::settings::load(app);
    let (tx, _rx) = watch::channel(0u64);
    AppState {
        settings: RwLock::new(settings),
        snapshot: RwLock::new(MonitorSnapshot::offline("正在连接后端…")),
        config_epoch_tx: tx,
        loaded_panel_url: Mutex::new(None),
    }
}
