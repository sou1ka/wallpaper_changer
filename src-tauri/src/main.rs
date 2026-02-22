#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex},
    time::Duration,
};

use chrono::{Datelike, Local, NaiveTime, Weekday};
use rand::{seq::SliceRandom, thread_rng};
use serde::{Deserialize, Serialize};
use tauri::{
    Manager,
    RunEvent,
    SystemTray,
    SystemTrayEvent,
    CustomMenuItem,
    SystemTrayMenu,
    SystemTrayMenuItem,
    WindowEvent,
    Size,
    LogicalSize,
};
use tokio::time::sleep;
use tokio::sync::Notify;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppConfig {
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default)]
    start_dt: Option<String>,
    #[serde(default)]
    end_dt: Option<String>,
    #[serde(default)]
    weekly: Option<Vec<String>>,
    #[serde(default)]
    monthly: Option<Vec<u32>>,
    #[serde(default)]
    default_wallpaper_path: Option<PathBuf>,
    #[serde(default)]
    file_targets: Vec<PathBuf>,
    #[serde(default = "default_random")]
    random: bool,
    // persisted window state (width/height in pixels and minimized flag)
    #[serde(default)]
    window_width: Option<u32>,
    #[serde(default)]
    window_height: Option<u32>,
    #[serde(default)]
    window_minimized: Option<bool>,
}

fn default_interval() -> u64 {
    60
}

fn default_random() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            interval: default_interval(),
            start_dt: None,
            end_dt: None,
            weekly: None,
            monthly: None,
            default_wallpaper_path: None,
            file_targets: Vec::new(),
            random: default_random(),
            window_width: None,
            window_height: None,
            window_minimized: None,
        }
    }
}

struct AppState {
    initial_wallpaper: Mutex<Option<PathBuf>>,
    config: Mutex<AppConfig>,
    random_active: Mutex<bool>,
    // remember what the last saved/known 'random' setting was so we can detect toggles
    last_random_enabled: Mutex<bool>,
    // when sequential mode is in use, track the next index to show
    current_index: Mutex<Option<usize>>,
    // remember last shown file (used to compute index when switching from random->sequential)
    last_shown: Mutex<Option<PathBuf>>,
    notify: Notify,
}

impl AppState {
    fn new(initial_wallpaper: Option<PathBuf>, config: AppConfig) -> Self {
        Self {
            initial_wallpaper: Mutex::new(initial_wallpaper),
            config: Mutex::new(config.clone()),
            random_active: Mutex::new(false),
            last_random_enabled: Mutex::new(config.random),
            current_index: Mutex::new(None),
            last_shown: Mutex::new(None),
            notify: Notify::new(),
        }
    }
}

fn load_config_from_exe_dir() -> AppConfig {
    let exe_path = std::env::current_exe().expect("failed to get current_exe");
    let exe_dir = exe_path.parent().unwrap();
    let config_path = exe_dir.join("config.json");

    if !config_path.exists() { // config.json が無い場合は default を作成して保存 ---
        eprintln!("config.json not found. Creating default config.");

        let default_cfg = AppConfig::default();
        if let Ok(json) = serde_json::to_string_pretty(&default_cfg) {
            let _ = std::fs::write(&config_path, json);
        }

        return default_cfg;
    }

    let content = match fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read config.json: {e}");
            return AppConfig::default();
        }
    };

    serde_json::from_str(&content).unwrap_or_else(|e| {
        eprintln!("failed to parse config.json: {e}");
        AppConfig::default()
    })
}

fn weekday_str_to_enum(s: &str) -> Option<Weekday> {
    match s.to_ascii_lowercase().as_str() {
        "sun" => Some(Weekday::Sun),
        "mon" => Some(Weekday::Mon),
        "tue" | "tues" => Some(Weekday::Tue),
        "wed" => Some(Weekday::Wed),
        "thu" | "thur" | "thurs" => Some(Weekday::Thu),
        "fri" => Some(Weekday::Fri),
        "sat" => Some(Weekday::Sat),
        _ => None,
    }
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

fn should_run(now: chrono::DateTime<Local>, cfg: &AppConfig) -> bool {
    if let Some(weekly) = &cfg.weekly {
        let today = now.weekday();
        if !weekly.iter().any(|w| weekday_str_to_enum(w) == Some(today)) {
            return false;
        }
    }

    if let Some(monthly) = &cfg.monthly {
        if !monthly.iter().any(|d| *d == now.day()) {
            return false;
        }
    }

    let time = now.time();

    if let Some(start_str) = &cfg.start_dt {
        if let Some(start_time) = parse_hhmm(start_str) {
            if time < start_time {
                return false;
            }
        }
    }

    if let Some(end_str) = &cfg.end_dt {
        if let Some(end_time) = parse_hhmm(end_str) {
            if time > end_time {
                return false;
            }
        }
    }

    true
}

fn get_current_wallpaper() -> Option<PathBuf> {
    match wallpaper::get() {
        Ok(path_str) => Some(PathBuf::from(path_str)),
        Err(e) => {
            eprintln!("failed to get current wallpaper: {e}");
            None
        }
    }
}

fn set_wallpaper(path: &Path) {
    //println!("set wallpaper: {}", path.to_string_lossy());
    if let Err(e) = wallpaper::set_from_path(path.to_string_lossy().as_ref()) {
        eprintln!("failed to set wallpaper: {e}");
    }
}

fn is_image_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "bmp" | "gif" | "webp"
        )
    } else {
        false
    }
}

fn collect_images_recursively(path: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();

    if path.is_file() {
        if is_image_file(path) {
            result.push(path.to_path_buf());
        }
    } else if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    result.extend(collect_images_recursively(&p));
                } else if is_image_file(&p) {
                    result.push(p);
                }
            }
        }
    }

    result
}

#[tauri::command]
fn save_config(app_handle: tauri::AppHandle, config: AppConfig) -> Result<(), String> {
    let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe_path.parent().ok_or("failed to get exe dir")?;
    let config_path = exe_dir.join("config.json");

    let mut merged = config.clone();
    if merged.file_targets.is_empty() && config_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(existing_cfg) = serde_json::from_str::<AppConfig>(&content) {
                if !existing_cfg.file_targets.is_empty() {
                    merged.file_targets = existing_cfg.file_targets;
                }
            }
        }
    }

    let json = serde_json::to_string_pretty(&merged).map_err(|e| format!("serialize error: {}", e))?;

    std::fs::write(&config_path, json).map_err(|e| format!("write error: {}", e))?;
    //println!("save: {} {:?}", config_path.display(), merged);
    let state = app_handle.state::<AppState>();
    {
        let mut cfg = state.config.lock().unwrap();
        *cfg = merged.clone();
        // also update the remembered last_random_enabled so the main loop can detect toggles
        let mut last_rand = state.last_random_enabled.lock().unwrap();
        *last_rand = merged.random;
    }

    state.notify.notify_one();

    Ok(())
}

#[tauri::command]
fn load_config_for_frontend() -> Result<AppConfig, String> {
    let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe_path.parent().ok_or("failed to get exe dir")?;
    let config_path = exe_dir.join("config.json");

    if !config_path.exists() {
        return Ok(AppConfig::default());
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read config.json: {}", e))?;

    let cfg: AppConfig = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse config.json: {}", e))?;

    Ok(cfg)
}

#[tauri::command]
fn add_file_targets(app_handle: tauri::AppHandle, paths: Vec<String>) -> Result<Vec<String>, String> {
    let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe_path.parent().ok_or("failed to get exe dir")?;
    let config_path = exe_dir.join("config.json");
    //println!("save path: {:?}", paths);
    // config.json を読み込み
    let mut cfg = if config_path.exists() {
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| format!("read error: {}", e))?;
        serde_json::from_str::<AppConfig>(&content).map_err(|e| format!("parse error: {}", e))?
    } else {
        AppConfig::default()
    };

    // 追加されたパスを展開
    let mut new_files = Vec::new();
    for p in paths {
        let path = PathBuf::from(&p);
        let imgs = collect_images_recursively(&path);
        for img in imgs {
            new_files.push(img.to_string_lossy().to_string());
        }
    }

    // 重複排除
    for f in new_files.iter() {
        if !cfg.file_targets.iter().any(|x| x.to_string_lossy() == *f) {
            cfg.file_targets.push(PathBuf::from(f));
        }
    }

    // 保存
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| format!("serialize error: {}", e))?;
    std::fs::write(&config_path, json).map_err(|e| format!("write error: {}", e))?;

    {
        let state = app_handle.state::<AppState>();
        let mut state_cfg = state.config.lock().unwrap();
        state_cfg.file_targets = cfg.file_targets.clone();
        state.notify.notify_one();
    }

    // フロントへ返す（文字列配列）
    Ok(cfg
        .file_targets
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect())
}

#[tauri::command]
fn remove_file_target(app_handle: tauri::AppHandle, path: String) -> Result<Vec<String>, String> {
    let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe_path.parent().ok_or("failed to get exe dir")?;
    let config_path = exe_dir.join("config.json");
    //println!("save path(remove): {}", path);

    let mut cfg = if config_path.exists() { // config.json を読み込み
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| format!("read error: {}", e))?;
        serde_json::from_str::<AppConfig>(&content).map_err(|e| format!("parse error: {}", e))?
    } else {
        AppConfig::default()
    };

    // 削除
    cfg.file_targets.retain(|p| p.to_string_lossy() != path);

    // 保存
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| format!("serialize error: {}", e))?;
    std::fs::write(&config_path, json).map_err(|e| format!("write error: {}", e))?;

    {
        let state = app_handle.state::<AppState>();
        let mut state_cfg = state.config.lock().unwrap();
        state_cfg.file_targets = cfg.file_targets.clone();
        state.notify.notify_one();
    }

    // 最新の fileTargets を返す
    Ok(cfg
        .file_targets
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect())
}

fn main() {
    let show = CustomMenuItem::new("show".to_string(), "表示");
    let quit = CustomMenuItem::new("quit".to_string(), "閉じる");
    let tray_menu = SystemTrayMenu::new()
        .add_item(show)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit);

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            save_config,
            load_config_for_frontend,
            add_file_targets,
            remove_file_target
        ])
        .setup(|app| {
            let initial_wallpaper = get_current_wallpaper();
            let config = load_config_from_exe_dir();

            // Attempt to restore window size and minimized state from config (if present).
            if let Some(win) = app.get_window("wallpaper_changer") {
                // restore size if both width and height are available
                if let (Some(w), Some(h)) = (config.window_width, config.window_height) {
                    // set_size expects logical sizes (f64)
                    let _ = win.set_size(Size::Logical(LogicalSize {
                        width: w as f64,
                        height: h as f64,
                    }));
                }
            }

            app.manage(AppState::new(initial_wallpaper, config));

            Ok(())
        })
        .system_tray(SystemTray::new().with_menu(tray_menu))
        .on_window_event(|event| {
            // handle window events to persist window size and minimized state immediately
            match event.event() {
                WindowEvent::Resized(_size) => {
                    //println!("window resize event");
                    // Save new size when window is resized (simplified, best-effort).
                    let win = event.window();
                    if let Ok(size) = win.inner_size() {
                        let width = size.width as u32;
                        let height = size.height as u32;
                        let minimized = win.is_minimized().unwrap_or(false);
                        if let Ok(exe_path) = std::env::current_exe() {
                            if let Some(exe_dir) = exe_path.parent() {
                                let config_path = exe_dir.join("config.json");
                                let mut cfg = if config_path.exists() {
                                    std::fs::read_to_string(&config_path)
                                        .ok()
                                        .and_then(|s| serde_json::from_str::<AppConfig>(&s).ok())
                                        .unwrap_or_else(AppConfig::default)
                                } else {
                                    AppConfig::default()
                                };
                                cfg.window_width = Some(width);
                                cfg.window_height = Some(height);
                                cfg.window_minimized = Some(minimized);
                                if let Ok(json) = serde_json::to_string_pretty(&cfg) {
                                    let _ = std::fs::write(&config_path, json);
                                    // update in-memory state
                                    let app_handle = win.app_handle();
                                    let state_ref = app_handle.state::<AppState>();
                                    let mut state_cfg = state_ref.config.lock().unwrap();
                                    *state_cfg = cfg;
                                }
                            }
                        }
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    //println!("window close event");
                    event.window().hide().unwrap();
                    api.prevent_close();
                }
                _ => {}
            }
        })
        .on_system_tray_event(|app, event| match event {
            SystemTrayEvent::DoubleClick {
                position: _,
                size: _,
                ..
            } => {
                let window = app.get_window("wallpaper_changer").unwrap();

                if window.is_visible().unwrap() {
                    window.hide().unwrap();
                } else {
                    window.unminimize().unwrap();
                    window.show().unwrap();
                    window.set_focus().unwrap();
                }
            }
            SystemTrayEvent::MenuItemClick { id, .. } => {
                match id.as_str() {
                    "show" => {
                        let window = app.get_window("wallpaper_changer").unwrap();
                        window.unminimize().unwrap();
                        window.show().unwrap();
                        window.set_focus().unwrap();
                    }
                    "quit" => {
                        // 終了処理を実行してからアプリを終了
                        let state_ref = app.state::<AppState>();
                        let initial = state_ref.initial_wallpaper.lock().unwrap().clone();
                        if let Some(path) = initial {
                            set_wallpaper(&path);
                        }
                        app.exit(0);
                    }
                    _ => {}
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            match event {
                RunEvent::Ready => {
                    let app_handle = app_handle.clone();

                    tauri::async_runtime::spawn(async move {
                        loop {
                            // --- 設定を読み出す ---
                            let (should_run_now, file_targets, initial_wallpaper, interval_secs, random_flag) = {
                                let state_ref = app_handle.state::<AppState>();

                                // config の取り出し
                                let cfg_cloned = {
                                    let cfg = state_ref.config.lock().unwrap();
                                    (
                                        cfg.file_targets.clone(),
                                        cfg.start_dt.clone(),
                                        cfg.end_dt.clone(),
                                        cfg.weekly.clone(),
                                        cfg.monthly.clone(),
                                        if cfg.interval == 0 { 60 } else { cfg.interval },
                                        cfg.random,
                                    )
                                };

                                let (file_targets, start_dt, end_dt, weekly, monthly, interval_secs, random_flag) =
                                    cfg_cloned;

                                // should_run 判定
                                let now = Local::now();
                                let run = {
                                    let mut tmp_cfg = AppConfig::default();
                                    tmp_cfg.file_targets = file_targets.clone();
                                    tmp_cfg.start_dt = start_dt;
                                    tmp_cfg.end_dt = end_dt;
                                    tmp_cfg.weekly = weekly;
                                    tmp_cfg.monthly = monthly;
                                    tmp_cfg.interval = interval_secs;
                                    should_run(now, &tmp_cfg)
                                };

                                // initial_wallpaper の取り出し
                                let initial_wallpaper = {
                                    let lock = state_ref.initial_wallpaper.lock().unwrap();
                                    lock.clone()
                                };

                                (run, file_targets, initial_wallpaper, interval_secs, random_flag)
                            };

                            // --- ランダム / 逐次処理 ---
                            let state_ref = app_handle.state::<AppState>();

                            if file_targets.is_empty() {
                                let mut active = state_ref.random_active.lock().unwrap();
                                if *active {
                                    if let Some(path) = initial_wallpaper.clone() {
                                        set_wallpaper(&path);
                                    }
                                    *active = false;
                                }
                                // clear index and last_shown when no targets
                                let mut idx_lock = state_ref.current_index.lock().unwrap();
                                *idx_lock = None;
                                let mut last_shown_lock = state_ref.last_shown.lock().unwrap();
                                *last_shown_lock = None;
                            } else {
                                let mut active = state_ref.random_active.lock().unwrap();

                                // grab tracking locks we need
                                let mut last_rand = state_ref.last_random_enabled.lock().unwrap();
                                let mut idx_lock = state_ref.current_index.lock().unwrap();
                                let mut last_shown_lock = state_ref.last_shown.lock().unwrap();

                                if should_run_now {
                                    *active = true;

                                    if random_flag {
                                        // random mode: pick randomly and remember last shown; clear sequential index
                                        let mut rng = thread_rng();
                                        if let Some(choice) = file_targets.choose(&mut rng) {
                                            set_wallpaper(choice);
                                            *last_shown_lock = Some(choice.clone());
                                        }
                                        *idx_lock = None;
                                        *last_rand = true;
                                    } else {
                                        // sequential mode: if we just toggled from random -> sequential,
                                        // start from the next index after the last shown image
                                        if *last_rand && idx_lock.is_none() {
                                            if let Some(last_path) = &*last_shown_lock {
                                                if let Some(pos) = file_targets.iter().position(|p| p == last_path) {
                                                    *idx_lock = Some((pos + 1) % file_targets.len());
                                                } else {
                                                    *idx_lock = Some(0);
                                                }
                                            } else if let Some(current) = get_current_wallpaper() {
                                                if let Some(pos) = file_targets.iter().position(|p| p == &current) {
                                                    *idx_lock = Some((pos + 1) % file_targets.len());
                                                } else {
                                                    *idx_lock = Some(0);
                                                }
                                            } else {
                                                *idx_lock = Some(0);
                                            }
                                        }

                                        // update remembered flag: we're now in sequential mode
                                        *last_rand = false;

                                        // sequential: ensure we have an index and show it
                                        if idx_lock.is_none() {
                                            *idx_lock = Some(0);
                                        }
                                        if let Some(i) = *idx_lock {
                                            let path = &file_targets[i % file_targets.len()];
                                            set_wallpaper(path);
                                            *last_shown_lock = Some(path.clone());
                                            *idx_lock = Some((i + 1) % file_targets.len());
                                        }
                                    }
                                } else {
                                    if *active {
                                        if let Some(path) = initial_wallpaper.clone() {
                                            set_wallpaper(&path);
                                        }
                                        *active = false;
                                    }
                                }
                            }

                            //println!("loop: {}", interval_secs);
                            // interval スリープ または 設定変更で早期再評価
                            tokio::select! {
                                _ = sleep(Duration::from_secs(interval_secs)) => {},
                                _ = state_ref.notify.notified() => {},
                            }
                        }
                    });
                }

                RunEvent::ExitRequested { .. } => {
                    // 終了時に壁紙を戻す処理
                    let state_ref = app_handle.state::<AppState>();
                    let initial = state_ref.initial_wallpaper.lock().unwrap().clone();
                    if let Some(path) = initial {
                        set_wallpaper(&path);
                    }
                }

                _ => {}
            }
        });
}
