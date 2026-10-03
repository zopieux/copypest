mod clipboard;
mod format_utils;
mod image_utils;
mod ipc;
mod model;
mod path_utils;
mod ui;
mod x11_utils;

use eframe::egui;

use clipboard::spawn_clipboard_manager;
use ipc::{
    IpcRequest, IpcResponse, get_ui_lock_path, is_daemon_running, run_ipc_server, send_ipc_request,
    subscribe_ipc,
};
use ui::CopypestApp;

fn ensure_daemon_running() -> Result<(), Box<dyn std::error::Error>> {
    if !is_daemon_running() {
        let exe = std::env::current_exe()?;
        std::process::Command::new(exe).arg("daemon").spawn()?;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if is_daemon_running() {
                return Ok(());
            }
        }
        return Err("Timed out waiting for copypest daemon to start".into());
    }
    Ok(())
}

struct UiLockGuard(std::path::PathBuf);
impl Drop for UiLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn focus_window_by_pid(target_pid: u32) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, CLIENT_MESSAGE_EVENT, ClientMessageData, ClientMessageEvent, ConfigureWindowAux,
        ConnectionExt as _, EventMask, InputFocus, StackMode,
    };

    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let Ok(net_client_list) = conn.intern_atom(false, b"_NET_CLIENT_LIST") else {
        return;
    };
    let Ok(net_client_list) = net_client_list.reply() else {
        return;
    };

    let Ok(net_wm_pid) = conn.intern_atom(false, b"_NET_WM_PID") else {
        return;
    };
    let Ok(net_wm_pid) = net_wm_pid.reply() else {
        return;
    };

    let Ok(net_active_window) = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW") else {
        return;
    };
    let Ok(net_active_window) = net_active_window.reply() else {
        return;
    };

    let mut windows = Vec::new();
    if let Ok(prop) =
        conn.get_property(false, root, net_client_list.atom, AtomEnum::WINDOW, 0, 1024)
        && let Ok(reply) = prop.reply()
        && let Some(wins) = reply.value32()
    {
        windows.extend(wins);
    }
    if windows.is_empty()
        && let Ok(tree) = conn.query_tree(root)
        && let Ok(tree_reply) = tree.reply()
    {
        windows.extend(tree_reply.children);
    }

    for win in windows {
        if let Ok(prop) = conn.get_property(false, win, net_wm_pid.atom, AtomEnum::CARDINAL, 0, 1)
            && let Ok(reply) = prop.reply()
            && let Some(mut pids) = reply.value32()
            && pids.next() == Some(target_pid)
        {
            let event = ClientMessageEvent {
                response_type: CLIENT_MESSAGE_EVENT,
                format: 32,
                sequence: 0,
                window: win,
                type_: net_active_window.atom,
                data: ClientMessageData::from([1, x11rb::CURRENT_TIME, 0, 0, 0]),
            };
            let _ = conn.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            );
            let _ =
                conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
            let _ = conn.set_input_focus(InputFocus::POINTER_ROOT, win, x11rb::CURRENT_TIME);
            let _ = conn.flush();
            break;
        }
    }
}

fn check_and_handle_existing_ui() -> Result<bool, Box<dyn std::error::Error>> {
    let lock_path = get_ui_lock_path();
    if lock_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&lock_path)
            && let Ok(pid) = content.trim().parse::<i32>()
        {
            unsafe {
                if libc::kill(pid, 0) == 0 {
                    focus_window_by_pid(pid as u32);
                    return Ok(true);
                }
            }
        }
        let _ = std::fs::remove_file(&lock_path);
    }
    Ok(false)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match cmd {
        "-h" | "--help" | "help" => {
            println!("copypest - minimal X11 clipboard manager\n");
            println!("Usage:");
            println!("  copypest           Open / focus UI window");
            println!("  copypest daemon    Run background headless daemon");
            return Ok(());
        }
        "daemon" => {
            let clipboard_handle = spawn_clipboard_manager()?;
            run_ipc_server(clipboard_handle)?;
            return Ok(());
        }
        "" => {
            if check_and_handle_existing_ui()? {
                return Ok(());
            }
        }
        other => {
            eprintln!("Unknown argument: {other}. Run with --help for usage.");
            std::process::exit(1);
        }
    }

    ensure_daemon_running()?;

    let lock_path = get_ui_lock_path();
    std::fs::write(&lock_path, std::process::id().to_string())?;
    let _lock_guard = UiLockGuard(lock_path);

    let items = match send_ipc_request(&IpcRequest::GetHistory)? {
        IpcResponse::History(items) => items,
        _ => vec![],
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("copypest")
            .with_decorations(false)
            .with_always_on_top()
            .with_inner_size([680.0, 440.0])
            .with_resizable(false)
            .with_visible(true),
        ..Default::default()
    };

    eframe::run_native(
        "copypest",
        native_options,
        Box::new(move |cc| {
            let (update_tx, update_rx) = std::sync::mpsc::channel();
            let ctx = cc.egui_ctx.clone();
            subscribe_ipc(move |items| {
                let _ = update_tx.send(items);
                ctx.request_repaint();
            });
            Ok(Box::new(CopypestApp::new(items, update_rx)))
        }),
    )?;

    Ok(())
}
