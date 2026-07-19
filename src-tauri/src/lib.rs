mod db;
mod playlist;
mod xtream;

use tauri::Manager;

/// macOS: mpv embeds video via Vulkan (MoltenVK). Point the bundled Vulkan
/// loader at our bundled MoltenVK ICD so `vo=gpu-next` can create a context.
/// Must run before libvulkan is loaded (i.e. before mpv init).
#[cfg(target_os = "macos")]
fn set_vulkan_icd() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let icd = dir.join("lib").join("MoltenVK_icd.json");
            if icd.exists() {
                std::env::set_var("VK_ICD_FILENAMES", &icd);
                std::env::set_var("VK_DRIVER_FILES", &icd);
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "macos")]
    set_vulkan_icd();

    tauri::Builder::default()
        .plugin(tauri_plugin_libmpv::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let store = db::open(app.handle()).expect("failed to initialize database");
            app.manage(store);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            db::add_m3u_source,
            db::add_xtream_source,
            db::refresh_source,
            db::remove_source,
            db::list_sources,
            db::search_channels,
            db::list_groups,
            db::toggle_favorite,
            db::list_favorites,
            db::record_play,
            db::list_recents,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
