mod backend;
mod events;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            backend::start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            backend::list_devices,
            backend::send_files,
            backend::send_text,
            backend::answer_receive,
            backend::set_alias,
            backend::set_destination,
            backend::get_config,
            backend::cancel_send,
            backend::cancel_receive,
            backend::refresh_devices,
            backend::add_device_ip,
            backend::open_received_dir,
            backend::copy_file_to,
            backend::open_file,
            backend::read_image_preview,
            backend::open_link,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
