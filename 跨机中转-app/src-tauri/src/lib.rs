mod backend;
mod events;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Android: core 的 tracing 日志写到 stderr,Tauri 会转到 logcat(RustStdoutStderr)
    #[cfg(target_os = "android")]
    let _ = tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).try_init();
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    #[cfg(target_os = "android")]
    let builder = builder.plugin(backend::picker_plugin());
    builder
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
            backend::paste_file_paths,
            backend::read_clipboard,
            backend::save_pasted_image,
            backend::start_drag_out,
            backend::pick_files,
            backend::pick_folder,
            backend::load_history,
            backend::save_history,
            backend::clear_history,
            backend::check_update,
            backend::open_update_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
