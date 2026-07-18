mod db;
mod playlist;
mod xtream;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
