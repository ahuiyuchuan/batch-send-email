//! Windows 原生宿主：只装载打包界面，读取与导出均通过本地命令完成。
//! 作者：ahui。没有本地 HTTP 服务；已保存凭据不返回页面、不写浏览器存储或日志。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod diagnostic_commands;
mod exclusion_commands;
mod history_commands;
mod mail_commands;
mod recipient_commands;
mod sender_account_commands;
mod session_commands;
mod state;
mod template_commands;
mod template_transfer_commands;
mod updates;

use std::sync::Arc;
use tauri::Manager;

/// 启动 Tauri/WebView2。QQ 登录仅在当前进程有效，启动时不恢复历史 Cookie。
fn main() {
    // 不记录panic正文；其中可能包含邮件或凭据。只保留源码位置，并避免panic时争抢日志锁。
    std::panic::set_hook(Box::new(|info| {
        member_core::diagnostics::record_panic(
            info.location()
                .map(|location| (location.file(), location.line())),
        );
    }));
    let state = state::AppState::new();
    let result = tauri::Builder::default()
        // 必须先判断单实例，再打开数据库和恢复中断状态，避免第二进程干扰正在投递的任务或重复发送。
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _working_directory| {
                member_core::diagnostics::event("INFO", "lifecycle", "second_instance", serde_json::json!({}));
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(state))
        .setup(|app| {
            // 日志先于WebView/数据库初始化，使启动失败也留有线索；失败只显示状态，不阻止业务启动。
            let data_directory = app.path().app_local_data_dir()?;
            let _ = member_core::diagnostics::initialize(&data_directory.join("logs"));
            member_core::diagnostics::event("INFO", "lifecycle", "application_start", serde_json::json!({
                "version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
                "build_id":env!("QQ_BUILD_ID").parse::<u64>().unwrap_or_default(),
                "log_format":1,"retention_files":5,"file_limit_mib":5,
            }));
            #[cfg(windows)]
            configure_windows_webview(app)?;
            let database_path = data_directory.join("data.sqlite3");
            let database = member_core::database::Database::open(database_path)?;
            let shared = app.state::<Arc<state::AppState>>();
            shared.initialize_database(database)?;
            // 更新仅访问固定GitHub公开发布信息，不等待网络且不持有业务锁。
            updates::start_monitor(app.handle().clone());
            member_core::diagnostics::event("INFO", "lifecycle", "application_ready", serde_json::json!({}));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            updates::get_update_status,
            updates::check_updates,
            updates::open_external_url,
            diagnostic_commands::get_log_status,
            diagnostic_commands::get_recent_logs,
            diagnostic_commands::open_log_directory,
            diagnostic_commands::report_frontend_diagnostic,
            diagnostic_commands::report_frontend_fault,
            exclusion_commands::get_manual_exclusions,
            exclusion_commands::add_manual_exclusions,
            exclusion_commands::remove_manual_exclusions,
            exclusion_commands::restore_filtered_members,
            exclusion_commands::get_snapshot_view,
            commands::get_app_state,
            commands::get_group_snapshot,
            commands::start_web_login,
            commands::poll_web_login,
            commands::cancel_web_login,
            commands::refresh_groups,
            commands::fetch_members,
            commands::cancel_fetch,
            commands::logout,
            commands::export_members,
            history_commands::get_sent_history,
            history_commands::add_sent_records,
            history_commands::import_sent_records,
            history_commands::remove_sent_record,
            history_commands::resolve_mail_delivery,
            history_commands::resolve_mail_deliveries,
            mail_commands::get_mail_state,
            mail_commands::get_member_mail_status,
            mail_commands::get_mail_settings,
            sender_account_commands::mail_accounts,
            sender_account_commands::mail_account_save,
            sender_account_commands::mail_account_remove,
            sender_account_commands::mail_accounts_reorder,
            sender_account_commands::mail_account_clear_password,
            mail_commands::save_mail_settings,
            mail_commands::clear_mail_password,
            mail_commands::mail_pick_attachments,
            mail_commands::mail_remove_attachment,
            mail_commands::mail_add_inline_image,
            mail_commands::mail_pick_inline_images,
            mail_commands::mail_remove_inline_image,
            template_commands::mail_template_list,
            template_commands::mail_template_preview,
            template_commands::mail_template_save,
            template_commands::mail_template_rename,
            template_commands::mail_template_duplicate,
            template_commands::mail_template_delete,
            template_commands::mail_template_apply,
            template_commands::mail_clear_draft_content,
            template_transfer_commands::mail_template_export,
            template_transfer_commands::mail_template_import_preview,
            template_transfer_commands::mail_template_import_commit,
            template_transfer_commands::mail_template_import_cancel,
            mail_commands::start_mail_job,
            recipient_commands::prepare_mail_recipients,
            recipient_commands::estimate_prepared_mail_completion,
            mail_commands::skip_mail_wait,
            mail_commands::cancel_mail_job
        ])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                member_core::diagnostics::event("INFO", "lifecycle", "window_destroyed", serde_json::json!({}));
                let state = window.state::<Arc<state::AppState>>();
                state.cancel_all();
            }
        })
        .run(tauri::generate_context!());
    if let Err(error) = result {
        // 启动错误仅使用已脱敏摘要，不能把任意调试对象递归序列化。
        member_core::diagnostics::event(
            "ERROR",
            "lifecycle",
            "startup_failed",
            serde_json::json!({"detail":member_core::diagnostics::sanitize(&error.to_string(), &[])}),
        );
        show_startup_error(&error.to_string());
    }
}

/// 关闭整个 WebView2 的浏览器默认菜单及开发工具，包括正文图片和预览 iframe。
///
/// 本函数仅在 setup 的 UI 线程调用；通过 Tauri 原生控制器设置，不拦截键盘、剪贴板或编辑。
/// with_webview 只返回调度结果，因此单独回传 COM 执行结果；设置失败即停止初始化，交由
/// 现有原生启动错误弹窗明确说明，不能把“已提交设置”误当作“菜单已关闭”。
#[cfg(windows)]
fn configure_windows_webview(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use std::{io, sync::mpsc, time::Duration};

    let window = app
        .get_webview_window("main")
        .ok_or_else(|| io::Error::other("主窗口未创建，无法设置 WebView2 右键菜单。"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    window.with_webview(move |webview| {
        // Tauri 在 WebView 所属 UI 线程执行闭包；控制器和 Settings 不跨线程保存。
        // 仅改两个明确设置，不关闭浏览器加速键，保留 Ctrl+C / Ctrl+V / Ctrl+Z 等编辑操作。
        let result = unsafe {
            webview
                .controller()
                .CoreWebView2()
                .and_then(|core| core.Settings())
                .and_then(|settings| {
                    settings.SetAreDefaultContextMenusEnabled(false)?;
                    settings.SetAreDevToolsEnabled(false)
                })
        }
        .map_err(|error| error.to_string());
        // 接收端只有初始化失败或退出时才会消失；不会重试或启用默认浏览器功能。
        let _ = sender.send(result);
    })?;
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| io::Error::other("WebView2 界面设置未完成，请关闭后重新打开程序。"))?
        .map_err(|detail| {
            io::Error::other(format!("无法关闭 WebView2 默认菜单或开发工具：{detail}"))
        })?;
    Ok(())
}

/// 启动失败时 WebView 尚不可用，使用 Windows 原生弹窗告知用户，避免 GUI 版本静默退出。
///
/// 错误仅来自宿主初始化，不包括业务请求或邮箱凭据；数据库文件必须保留，不能为了启动自动删除。
#[cfg(target_os = "windows")]
fn show_startup_error(detail: &str) {
    use std::{ffi::c_void, iter::once};

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut c_void,
            text: *const u16,
            caption: *const u16,
            options: u32,
        ) -> i32;
    }

    let message = format!(
        "无法启动batch-send-email。\n\n请确认 Microsoft Edge WebView2 运行时可用，并检查本地数据目录的访问权限。\n请勿删除数据库，可关闭其他实例后重试。\n\n错误详情：{detail}"
    );
    let text: Vec<u16> = message
        .replace('\0', " ")
        .encode_utf16()
        .chain(once(0))
        .collect();
    let caption: Vec<u16> = "启动失败".encode_utf16().chain(once(0)).collect();
    // UTF-16 缓冲区在调用期间有效；无父窗口，MB_ICONERROR | MB_OK 仅显示故障信息。
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x10);
    }
}

/// 非 Windows 开发环境没有原生 Windows 弹窗，保留可见的标准错误输出供定位。
#[cfg(not(target_os = "windows"))]
fn show_startup_error(detail: &str) {
    eprintln!("无法启动batch-send-email：{detail}");
}
