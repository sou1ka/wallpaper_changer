#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use chrono::{Datelike, Local, NaiveTime, Weekday};
use rand::{seq::SliceRandom, thread_rng};
use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    LogicalSize, Manager, RunEvent, Size, WindowEvent,
};
use tokio::sync::Notify;
use tokio::time::sleep;

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

    if !config_path.exists() {
        // config.json が無い場合は default を作成して保存 ---
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

fn should_run(now: chrono::DateTime<Local>, cfg: &AppConfig, currently_active: bool) -> bool {
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
    let start = cfg.start_dt.as_deref().and_then(parse_hhmm);
    let end = cfg.end_dt.as_deref().and_then(parse_hhmm);

    if currently_active {
        // 動作中: 終了条件のみチェック
        if let Some(end_time) = end {
            let stop = match start {
                Some(start_time) if start_time > end_time => {
                    // 日またぎ: [end_time, start_time) のギャップにいれば停止
                    time > end_time && time < start_time
                }
                _ => {
                    // 同日: end_time を過ぎたら停止
                    time > end_time
                }
            };
            if stop {
                return false;
            }
        }
    } else {
        // 停止中: 開始ウィンドウに入っているかチェック
        match (start, end) {
            (Some(s), Some(e)) if s <= e => {
                // 同日範囲 [s, e]
                if time < s || time > e {
                    return false;
                }
            }
            (Some(s), Some(e)) => {
                // 日またぎ範囲: time >= s OR time <= e の外なら停止
                if time < s && time > e {
                    return false;
                }
            }
            (Some(s), None) => {
                // 開始のみ: s 以降であれば動作
                if time < s {
                    return false;
                }
            }
            _ => {
                // 条件なし: 常に動作
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

    let json =
        serde_json::to_string_pretty(&merged).map_err(|e| format!("serialize error: {}", e))?;

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
fn add_file_targets(
    app_handle: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
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

    let mut cfg = if config_path.exists() {
        // config.json を読み込み
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
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            save_config,
            load_config_for_frontend,
            add_file_targets,
            remove_file_target
        ])
        .setup(|app| {
            let initial_wallpaper = get_current_wallpaper();
            let config = load_config_from_exe_dir();

            if let Some(win) = app.get_webview_window("wallpaper_changer") {
                if let (Some(w), Some(h)) = (config.window_width, config.window_height) {
                    let _ = win.set_size(Size::Logical(LogicalSize {
                        width: w as f64,
                        height: h as f64,
                    }));
                }
            }

            app.manage(AppState::new(initial_wallpaper, config));

            // Tauri v2 システムトレイ
            let show_item = MenuItem::with_id(app, "show", "表示", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "閉じる", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[
                &show_item,
                &PredefinedMenuItem::separator(app)?,
                &quit_item,
            ])?;

            let tray = TrayIconBuilder::new()
                .menu(&menu)
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("wallpaper_changer") {
                            let _ = window.unminimize();
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        let state_ref = app.state::<AppState>();
                        let initial = state_ref.initial_wallpaper.lock().unwrap().clone();
                        if let Some(path) = initial {
                            set_wallpaper(&path);
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("wallpaper_changer") {
                            let is_visible = window.is_visible().unwrap_or(false);
                            let is_minimized = window.is_minimized().unwrap_or(false);
                            if is_visible && !is_minimized {
                                // 通常表示中 → 非表示（トレイに格納）
                                let _ = window.hide();
                            } else {
                                // 非表示 or 最小化中 → 復元
                                let _ = window.unminimize();
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // TrayIcon を管理下に置いてアプリ終了まで生存させる
            app.manage(tray);

            Ok(())
        })
        .on_window_event(|window, event: &WindowEvent| {
            match event {
                WindowEvent::Resized(_size) => {
                    // 最小化中は不正なサイズが取得されるためスキップ
                    if window.is_minimized().unwrap_or(false) {
                        return;
                    }
                    if let Ok(size) = window.inner_size() {
                        let width = size.width;
                        let height = size.height;
                        let minimized = window.is_minimized().unwrap_or(false);
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
                                    let app_handle = window.app_handle();
                                    let state_ref = app_handle.state::<AppState>();
                                    let mut state_cfg = state_ref.config.lock().unwrap();
                                    *state_cfg = cfg;
                                }
                            }
                        }
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    let _ = window.hide();
                    api.prevent_close();
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle: &tauri::AppHandle, event| {
            match event {
                RunEvent::Ready => {
                    let app_handle = app_handle.clone();

                    tauri::async_runtime::spawn(async move {
                        loop {
                            // --- 設定を読み出す ---
                            let (
                                should_run_now,
                                file_targets,
                                initial_wallpaper,
                                interval_secs,
                                random_flag,
                            ) = {
                                let state_ref = app_handle.state::<AppState>();

                                // 現在の動作状態を先読み（should_run の判定に使う）
                                let currently_active = *state_ref.random_active.lock().unwrap();

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

                                let (
                                    file_targets,
                                    start_dt,
                                    end_dt,
                                    weekly,
                                    monthly,
                                    interval_secs,
                                    random_flag,
                                ) = cfg_cloned;

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
                                    should_run(now, &tmp_cfg, currently_active)
                                };

                                // initial_wallpaper の取り出し
                                let initial_wallpaper = {
                                    let lock = state_ref.initial_wallpaper.lock().unwrap();
                                    lock.clone()
                                };

                                (
                                    run,
                                    file_targets,
                                    initial_wallpaper,
                                    interval_secs,
                                    random_flag,
                                )
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
                                                if let Some(pos) =
                                                    file_targets.iter().position(|p| p == last_path)
                                                {
                                                    *idx_lock =
                                                        Some((pos + 1) % file_targets.len());
                                                } else {
                                                    *idx_lock = Some(0);
                                                }
                                            } else if let Some(current) = get_current_wallpaper() {
                                                if let Some(pos) =
                                                    file_targets.iter().position(|p| p == &current)
                                                {
                                                    *idx_lock =
                                                        Some((pos + 1) % file_targets.len());
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

                            // 最大60秒ごとに時刻を再チェック（開始・終了の検出遅延を60秒以内に抑える）
                            let sleep_secs = interval_secs.min(60);
                            tokio::select! {
                                _ = sleep(Duration::from_secs(sleep_secs)) => {},
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
