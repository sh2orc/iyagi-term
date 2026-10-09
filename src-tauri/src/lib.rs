//! Tauri shell. Owns the window and bridges frontend calls to the local
//! daemon over the framed IPC contract; never owns PTYs directly.

mod bridge;
mod quit;

use bridge::commands::{
    bridge_ack, bridge_connect, bridge_connection_status, bridge_disconnect, bridge_open_data,
    bridge_revision, bridge_rpc, bridge_subscribe_events, bridge_subscribe_session,
    bridge_unsubscribe_session, claude_hooks_apply, claude_hooks_remove, claude_hooks_status,
    claude_usage_apply, claude_usage_remove, claude_usage_status, codex_hooks_apply,
    codex_hooks_remove, codex_hooks_status, shell_profiles_apply, shell_profiles_remove,
    shell_profiles_status, subscription_usage_refresh, system_git_branch, system_list_clis,
    system_list_shells, system_locate_program, system_query_version, zai_api_key_remove,
    zai_api_key_set, zai_api_key_status,
};
use bridge::ime::{ime_set_hangul, ime_state, ime_toggle_hangul};
use bridge::paste_image::paste_save_image;
use quit::{app_quit, app_quit_ack, app_quit_cancel};

/// `windows_sys` bundles each DLL's externs into a single object, so linking
/// any Windows feature (tokio named pipes here) drags a `TaskDialogIndirect`
/// import into every binary. The real app satisfies it through the
/// Common-Controls v6 manifest tauri-build embeds into the app exe; headless
/// TEST binaries get no manifest, and the loader then binds comctl32 v5,
/// which lacks that export (`STATUS_ENTRYPOINT_NOT_FOUND` at process start).
/// Unit tests never open task dialogs, so a local no-op definition satisfies
/// the loader without any manifest tooling.
#[cfg(all(test, windows))]
#[no_mangle]
extern "system" fn TaskDialogIndirect(
    _config: *const core::ffi::c_void,
    _button: *mut i32,
    _radio: *mut i32,
    _verification_flag_checked: *mut i32,
) -> i32 {
    0 // S_OK; unused by headless tests.
}

/// 트레이 "열기"로 창을 표시하고, "종료"로 앱 종료를 요청한다.
/// 창 닫기와 트레이 "종료"는 모두 `app.exit` → `ExitRequested`로 들어와 quit.rs가 가로채어
/// 살아 있는 터미널을 함께 끝낼지 프론트에 먼저 묻는다.
fn setup_tray(app: &tauri::App) {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    // 트레이 메뉴는 Rust 쪽에 있어 프론트 i18n를 못 쓴다 — 이중 표기.
    let open =
        MenuItem::with_id(app, "open", "열기 · Open", true, None::<&str>).expect("tray open item");
    let quit =
        MenuItem::with_id(app, "quit", "종료 · Quit", true, None::<&str>).expect("tray quit item");
    let menu = Menu::with_items(app, &[&open, &quit]).expect("tray menu");

    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 트레이 아이콘 왼쪽 클릭 = 열기(드롭다운 대신 즉시 복귀).
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    // 트레이 등록 실패는 앱을 죽이지 않는다 — 창만 쓰는 종전 동작 유지.
    if let Err(error) = builder.build(app) {
        eprintln!("iyagi: tray unavailable ({error}) — window-only mode");
    }
}

/// 메인 창을 보이게 하고 포커스를 돌린다(트레이 "열기"·아이콘 클릭 공용).
fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Refresh the per-user daemon copy that the baked paths (ccd/ccg, CLI
    // hooks) point at — heals a moved/renamed checkout just by launching the
    // app, and is a no-op when nothing changed (stamp check).
    bridge::daemon_manager::refresh_stable_daemon_copy();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(bridge::state::BridgeState::new())
        .manage(quit::QuitState::default())
        .invoke_handler(tauri::generate_handler![
            bridge_connect,
            bridge_rpc,
            bridge_open_data,
            bridge_subscribe_session,
            bridge_unsubscribe_session,
            bridge_subscribe_events,
            bridge_ack,
            bridge_disconnect,
            bridge_connection_status,
            bridge_revision,
            system_list_clis,
            system_list_shells,
            system_git_branch,
            system_query_version,
            system_locate_program,
            ime_state,
            ime_toggle_hangul,
            ime_set_hangul,
            paste_save_image,
            claude_hooks_status,
            claude_hooks_apply,
            claude_hooks_remove,
            codex_hooks_status,
            codex_hooks_apply,
            codex_hooks_remove,
            claude_usage_status,
            claude_usage_apply,
            claude_usage_remove,
            subscription_usage_refresh,
            zai_api_key_status,
            zai_api_key_set,
            zai_api_key_remove,
            shell_profiles_status,
            shell_profiles_apply,
            shell_profiles_remove,
            app_quit,
            app_quit_ack,
            app_quit_cancel
        ])
        .setup(|app| {
            setup_tray(app);
            Ok(())
        })
        .on_window_event(|window, event| {
            // macOS 빨간 닫기 버튼도 트레이 종료와 같은 확인 흐름을 거친다.
            // 웹뷰를 먼저 닫으면 확인창을 띄울 수 없으므로 결정까지 유지한다.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                use tauri::Manager as _;
                api.prevent_close();
                window.app_handle().exit(0);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building iyagi")
        .run(|app, event| {
            // 종료 요청(트레이 "종료"·마지막 창 파괴)은 승인 전엔 막고
            // 프론트에 묻는다 — 살아 있는 터미널을 함께 끝낼지는 사용자 몫.
            match &event {
                tauri::RunEvent::ExitRequested { api, code, .. } => {
                    quit::on_exit_requested(app, api, *code);
                }
                _ => {
                    // macOS: Dock icon click while the window is tray-hidden
                    // (Windows/Linux restore via the tray menu).
                    #[cfg(target_os = "macos")]
                    if let tauri::RunEvent::Reopen {
                        has_visible_windows: false,
                        ..
                    } = event
                    {
                        show_main_window(app);
                    }
                }
            }
        });
}
