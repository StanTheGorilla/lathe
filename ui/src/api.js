// Thin wrapper over the Tauri commands the core exposes. Brief section 3: the settings
// window talks to the resident core over the existing IPC.
import { invoke } from "@tauri-apps/api/core";

export const loadConfig = () => invoke("load_config");
export const saveConfig = (config) => invoke("save_config", { config });
export const listDevices = () => invoke("list_devices");
export const modelStatus = () => invoke("model_status");
export const inputLevel = () => invoke("input_level");
export const startLevelMeter = () => invoke("start_level_meter");
export const stopLevelMeter = () => invoke("stop_level_meter");
export const runBenchmark = () => invoke("run_benchmark");
export const configPath = () => invoke("config_path");
export const historyRecent = (search, limit) => invoke("history_recent", { search, limit });
export const historyStats = () => invoke("history_stats");
export const historyWipe = () => invoke("history_wipe");
export const historyPaste = (id, raw) => invoke("history_paste", { id, raw });
export const autostartEnabled = () => invoke("autostart_enabled");
export const setAutostart = (enabled) => invoke("set_autostart", { enabled });
export const downloadableModels = () => invoke("downloadable_models");
export const downloadProgress = () => invoke("download_progress");
export const startDownload = (file) => invoke("start_download", { file });
export const cancelDownload = () => invoke("cancel_download");
export const lastHotkey = () => invoke("last_hotkey");
