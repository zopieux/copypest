use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use crate::model::{ClipboardCommand, ClipboardHandle, HistoryItem};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum IpcRequest {
    GetHistory,
    RestoreNative(u64),
    RestorePlainText(u64),
    RestorePathText(u64),
    DeleteItem(u64),
    Ping,
    Subscribe,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum IpcResponse {
    History(Vec<HistoryItem>),
    Ok,
}

pub fn get_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("copypest.sock")
    } else {
        let uid = libc_getuid();
        PathBuf::from(format!("/tmp/copypest-{uid}.sock"))
    }
}

pub fn get_ui_lock_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("copypest-ui.lock")
    } else {
        let uid = libc_getuid();
        PathBuf::from(format!("/tmp/copypest-ui-{uid}.lock"))
    }
}

fn libc_getuid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.uid()
        })
        .unwrap_or(1000)
}

pub fn is_daemon_running() -> bool {
    send_ipc_request(&IpcRequest::Ping).is_ok()
}

pub fn send_ipc_request(req: &IpcRequest) -> Result<IpcResponse, Box<dyn std::error::Error>> {
    let path = get_socket_path();
    let mut stream = UnixStream::connect(&path)?;
    bincode::serialize_into(&mut stream, req)?;
    let resp: IpcResponse = bincode::deserialize_from(&mut stream)?;
    Ok(resp)
}

pub fn subscribe_ipc(on_update: impl Fn(Vec<HistoryItem>) + Send + 'static) {
    std::thread::spawn(move || {
        let path = get_socket_path();
        if let Ok(mut stream) = UnixStream::connect(&path)
            && bincode::serialize_into(&mut stream, &IpcRequest::Subscribe).is_ok()
        {
            while let Ok(IpcResponse::History(items)) =
                bincode::deserialize_from::<_, IpcResponse>(&mut stream)
            {
                on_update(items);
            }
        }
    });
}

pub fn run_ipc_server(handle: ClipboardHandle) -> Result<(), Box<dyn std::error::Error>> {
    let path = get_socket_path();
    if path.exists() {
        if is_daemon_running() {
            return Err("Another instance of copypest daemon is already running".into());
        }
        let _ = fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)?;

    for mut stream in listener.incoming().flatten() {
        let handle = handle.clone();
        std::thread::spawn(move || {
            if let Ok(req) = bincode::deserialize_from::<_, IpcRequest>(&mut stream) {
                match req {
                    IpcRequest::Subscribe => {
                        let (tx, rx) = std::sync::mpsc::channel::<()>();
                        {
                            let mut st = handle.state.lock().unwrap();
                            st.notify_tx.push(tx);
                        }
                        use std::io::Write;
                        loop {
                            let items = {
                                let st = handle.state.lock().unwrap();
                                st.history.clone()
                            };
                            let resp = IpcResponse::History(items);
                            if bincode::serialize_into(&mut stream, &resp).is_err() {
                                break;
                            }
                            if stream.flush().is_err() {
                                break;
                            }
                            if rx.recv().is_err() {
                                break;
                            }
                        }
                    }
                    IpcRequest::GetHistory => {
                        let items = {
                            let st = handle.state.lock().unwrap();
                            st.history.clone()
                        };
                        let resp = IpcResponse::History(items);
                        let _ = bincode::serialize_into(&mut stream, &resp);
                    }
                    IpcRequest::RestoreNative(id) => {
                        let _ = handle.tx.send(ClipboardCommand::RestoreNative(id));
                        let _ = bincode::serialize_into(&mut stream, &IpcResponse::Ok);
                    }
                    IpcRequest::RestorePlainText(id) => {
                        let _ = handle.tx.send(ClipboardCommand::RestorePlainText(id));
                        let _ = bincode::serialize_into(&mut stream, &IpcResponse::Ok);
                    }
                    IpcRequest::RestorePathText(id) => {
                        let _ = handle.tx.send(ClipboardCommand::RestorePathText(id));
                        let _ = bincode::serialize_into(&mut stream, &IpcResponse::Ok);
                    }
                    IpcRequest::DeleteItem(id) => {
                        let _ = handle.tx.send(ClipboardCommand::DeleteItem(id));
                        let _ = bincode::serialize_into(&mut stream, &IpcResponse::Ok);
                    }
                    IpcRequest::Ping => {
                        let _ = bincode::serialize_into(&mut stream, &IpcResponse::Ok);
                    }
                }
            }
        });
    }

    Ok(())
}
