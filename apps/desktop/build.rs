fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "get_status",
            "get_diagnostic_status",
            "update_settings",
            "create_invitation",
            "copy_invitation",
            "pair_device",
            "forget_peer",
            "reconnect",
            "emergency_stop",
            "send_file",
            "respond_file",
            "diagnostics",
            "export_diagnostics",
            "open_log_folder",
            "open_export_folder",
            "open_project_url",
            "add_peer_firewall_rule",
        ]),
    ))
    .expect("Unable to build desktop capabilities");
}
