use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{self, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::image_utils::process_image_payload;
pub use crate::model::*;
use crate::path_utils::{compute_disk_stats, uri_to_path};
use crate::x11_utils::{Atoms, ServedData, convert_and_fetch_target, intern_all_atoms};

const MAX_HISTORY_ITEMS: usize = 100;

pub fn spawn_clipboard_manager() -> Result<ClipboardHandle, Box<dyn std::error::Error>> {
    let state = Arc::new(Mutex::new(SharedClipboardState::new()));
    let (tx, rx) = channel::<ClipboardCommand>();

    let state_clone = Arc::clone(&state);
    thread::spawn(move || {
        if let Err(e) = run_clipboard_loop(state_clone, rx) {
            eprintln!("Clipboard loop exited with error: {e}");
        }
    });

    Ok(ClipboardHandle { state, tx })
}

fn run_clipboard_loop(
    state: Arc<Mutex<SharedClipboardState>>,
    rx: Receiver<ClipboardCommand>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let atoms = intern_all_atoms(&conn)?;

    let helper_window = conn.generate_id()?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        helper_window,
        root,
        -100,
        -100,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;

    xfixes::query_version(&conn, 5, 0)?.reply()?;
    xfixes::select_selection_input(
        &conn,
        root,
        atoms.clipboard,
        SelectionEventMask::SET_SELECTION_OWNER
            | SelectionEventMask::SELECTION_WINDOW_DESTROY
            | SelectionEventMask::SELECTION_CLIENT_CLOSE,
    )?;
    conn.flush()?;

    let mut next_id: u64 = 1;
    let mut served_data: Option<ServedData> = None;

    loop {
        while let Ok(cmd) = rx.try_recv() {
            handle_clipboard_command(&conn, &atoms, helper_window, &state, &mut served_data, cmd);
        }

        let mut had_event = false;
        while let Ok(Some(event)) = conn.poll_for_event() {
            had_event = true;
            match event {
                Event::XfixesSelectionNotify(ev) => {
                    if ev.selection == atoms.clipboard && ev.owner != 0 && ev.owner != helper_window
                    {
                        handle_new_clipboard_selection(
                            &conn,
                            &atoms,
                            helper_window,
                            &state,
                            &mut next_id,
                        );
                    }
                }
                Event::SelectionRequest(req) => {
                    if req.selection == atoms.clipboard {
                        handle_selection_request(&conn, &atoms, &req, served_data.as_ref());
                    }
                }
                Event::SelectionClear(ev) if ev.selection == atoms.clipboard => {
                    served_data = None;
                }
                _ => {}
            }
        }

        if !had_event {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn build_native_served_data(
    conn: &RustConnection,
    atoms: &Atoms,
    item: &HistoryItem,
) -> ServedData {
    let mut payloads: HashMap<Atom, (Atom, Vec<u8>)> = HashMap::new();
    let mut target_atoms = vec![atoms.targets];

    for (name, bytes) in &item.targets {
        if let Ok(reply) = conn.intern_atom(false, name.as_bytes())
            && let Ok(reply) = reply.reply()
        {
            let target_atom = reply.atom;
            let type_atom = if name.starts_with("image/") {
                target_atom
            } else if name == "UTF8_STRING" || name == "text/plain;charset=utf-8" {
                atoms.utf8_string
            } else {
                atoms.string
            };
            payloads.insert(target_atom, (type_atom, bytes.clone()));
            if !target_atoms.contains(&target_atom) {
                target_atoms.push(target_atom);
            }
        }
    }

    if let Some(ref text) = item.plain_text {
        let bytes = text.as_bytes().to_vec();
        payloads.insert(atoms.utf8_string, (atoms.utf8_string, bytes.clone()));
        payloads.insert(atoms.string, (atoms.string, bytes));
        if !target_atoms.contains(&atoms.utf8_string) {
            target_atoms.push(atoms.utf8_string);
        }
        if !target_atoms.contains(&atoms.string) {
            target_atoms.push(atoms.string);
        }
    }

    ServedData {
        target_atoms,
        payloads,
    }
}

fn build_text_served_data(atoms: &Atoms, text: &str) -> ServedData {
    let bytes = text.as_bytes().to_vec();
    let mut payloads: HashMap<Atom, (Atom, Vec<u8>)> = HashMap::new();
    payloads.insert(atoms.utf8_string, (atoms.utf8_string, bytes.clone()));
    payloads.insert(atoms.string, (atoms.string, bytes.clone()));
    payloads.insert(atoms.text, (atoms.string, bytes.clone()));
    payloads.insert(atoms.text_plain, (atoms.utf8_string, bytes.clone()));
    payloads.insert(atoms.text_plain_utf8, (atoms.utf8_string, bytes));

    let target_atoms = vec![
        atoms.targets,
        atoms.utf8_string,
        atoms.string,
        atoms.text,
        atoms.text_plain,
        atoms.text_plain_utf8,
    ];

    ServedData {
        target_atoms,
        payloads,
    }
}

fn set_active_selection(
    conn: &RustConnection,
    atoms: &Atoms,
    helper_window: Window,
    state: &Arc<Mutex<SharedClipboardState>>,
    served_data: &mut Option<ServedData>,
    new_served: ServedData,
    item: HistoryItem,
) {
    *served_data = Some(new_served);

    let mut st = state.lock().unwrap();
    st.last_copied_id = Some(item.id);
    st.history.insert(0, item);
    st.notify_subscribers();

    let _ = conn.set_selection_owner(helper_window, atoms.clipboard, x11rb::CURRENT_TIME);
    let _ = conn.flush();
}

fn handle_clipboard_command(
    conn: &RustConnection,
    atoms: &Atoms,
    helper_window: Window,
    state: &Arc<Mutex<SharedClipboardState>>,
    served_data: &mut Option<ServedData>,
    cmd: ClipboardCommand,
) {
    match cmd {
        ClipboardCommand::RestoreNative(id) => {
            let mut st = state.lock().unwrap();
            if st.last_copied_id == Some(id) {
                return;
            }
            if let Some(idx) = st.history.iter().position(|it| it.id == id) {
                let item = st.history.remove(idx);
                let served = build_native_served_data(conn, atoms, &item);
                drop(st);
                set_active_selection(conn, atoms, helper_window, state, served_data, served, item);
            }
        }
        ClipboardCommand::RestorePlainText(id) => {
            let mut st = state.lock().unwrap();
            if let Some(idx) = st.history.iter().position(|it| it.id == id) {
                let item = st.history.remove(idx);
                if let Some(ref text) = item.plain_text {
                    let served = build_text_served_data(atoms, text);
                    drop(st);
                    set_active_selection(
                        conn,
                        atoms,
                        helper_window,
                        state,
                        served_data,
                        served,
                        item,
                    );
                } else {
                    st.history.insert(idx, item);
                }
            }
        }
        ClipboardCommand::RestorePathText(id) => {
            let mut st = state.lock().unwrap();
            if let Some(idx) = st.history.iter().position(|it| it.id == id) {
                let item = st.history.remove(idx);
                let path_text = if let Some(ref paths) = item.uri_paths {
                    paths.join("\n")
                } else if let Some(ref pt) = item.plain_text {
                    pt.clone()
                } else {
                    String::new()
                };
                let served = build_text_served_data(atoms, &path_text);
                drop(st);
                set_active_selection(conn, atoms, helper_window, state, served_data, served, item);
            }
        }
        ClipboardCommand::DeleteItem(id) => {
            let mut st = state.lock().unwrap();
            st.history.retain(|it| it.id != id);
            if st.last_copied_id == Some(id) {
                st.last_copied_id = None;
                *served_data = None;
            }
            st.notify_subscribers();
        }
    }
}

fn handle_selection_request(
    conn: &RustConnection,
    atoms: &Atoms,
    req: &SelectionRequestEvent,
    served_data: Option<&ServedData>,
) {
    let mut property = if req.property == 0 {
        req.target
    } else {
        req.property
    };

    if let Some(served) = served_data {
        if req.target == atoms.targets {
            let mut raw_atoms = Vec::with_capacity(served.target_atoms.len());
            for a in &served.target_atoms {
                raw_atoms.extend_from_slice(&a.to_ne_bytes());
            }
            let _ = conn.change_property(
                PropMode::REPLACE,
                req.requestor,
                property,
                AtomEnum::ATOM,
                32,
                served.target_atoms.len() as u32,
                &raw_atoms,
            );
        } else if let Some((type_atom, bytes)) = served.payloads.get(&req.target) {
            let _ = conn.change_property(
                PropMode::REPLACE,
                req.requestor,
                property,
                *type_atom,
                8,
                bytes.len() as u32,
                bytes,
            );
        } else {
            property = 0;
        }
    } else {
        property = 0;
    }

    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property,
    };
    let _ = conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
    let _ = conn.flush();
}

fn fetch_available_mime_names(
    conn: &RustConnection,
    atoms: &Atoms,
    helper_window: Window,
) -> Option<Vec<String>> {
    let targets_bytes = convert_and_fetch_target(
        conn,
        helper_window,
        atoms.clipboard,
        atoms.targets,
        atoms.transfer_prop,
        atoms.incr,
    )?;

    let (chunks, _) = targets_bytes.as_chunks::<4>();
    let available_atoms: Vec<Atom> = chunks.iter().map(|&c| u32::from_ne_bytes(c)).collect();

    let mut mime_names = Vec::new();
    for a in &available_atoms {
        if let Ok(reply) = conn.get_atom_name(*a)
            && let Ok(name_reply) = reply.reply()
            && let Ok(name_str) = String::from_utf8(name_reply.name)
        {
            mime_names.push(name_str);
        }
    }
    Some(mime_names)
}

struct RawClipboardPayload {
    kind: ItemKind,
    primary_bytes: Vec<u8>,
    fetched_targets: HashMap<String, Vec<u8>>,
    thumbnail: Option<Thumbnail>,
    image_dimensions: Option<(u32, u32)>,
    plain_text_str: Option<String>,
    uri_paths_vec: Option<Vec<String>>,
    is_rich: bool,
}

fn fetch_clipboard_payload(
    conn: &RustConnection,
    atoms: &Atoms,
    helper_window: Window,
    mime_names: &[String],
) -> Option<RawClipboardPayload> {
    let has_png = mime_names.iter().any(|m| m == "image/png");
    let has_jpeg = mime_names.iter().any(|m| m == "image/jpeg");
    let has_webp = mime_names.iter().any(|m| m == "image/webp");
    let has_bmp = mime_names.iter().any(|m| m == "image/bmp");
    let has_uri = mime_names.iter().any(|m| m == "text/uri-list");
    let has_gnome = mime_names
        .iter()
        .any(|m| m == "x-special/gnome-copied-files");
    let has_html = mime_names.iter().any(|m| m == "text/html");
    let has_utf8 = mime_names.iter().any(|m| m == "UTF8_STRING");
    let has_string = mime_names.iter().any(|m| m == "STRING");

    let mut fetched_targets: HashMap<String, Vec<u8>> = HashMap::new();
    let mut kind = ItemKind::PlainText;
    let mut primary_bytes = Vec::new();
    let mut thumbnail = None;
    let mut image_dimensions = None;
    let mut plain_text_str = None;
    let mut uri_paths_vec = None;
    let mut is_rich = false;

    if has_png || has_jpeg || has_webp || has_bmp {
        let (img_target, img_name) = if has_png {
            (atoms.image_png, "image/png")
        } else if has_jpeg {
            (atoms.image_jpeg, "image/jpeg")
        } else if has_webp {
            (atoms.image_webp, "image/webp")
        } else {
            (atoms.image_bmp, "image/bmp")
        };
        if let Some(img_data) = convert_and_fetch_target(
            conn,
            helper_window,
            atoms.clipboard,
            img_target,
            atoms.transfer_prop,
            atoms.incr,
        ) {
            primary_bytes = img_data.clone();
            fetched_targets.insert(img_name.to_string(), img_data.clone());
            kind = ItemKind::Image;

            let (dims, thumb) = process_image_payload(&img_data);
            image_dimensions = dims;
            thumbnail = thumb;
        }
    } else if has_uri || has_gnome {
        kind = ItemKind::UriList;
        if let Some(uri_data) = convert_and_fetch_target(
            conn,
            helper_window,
            atoms.clipboard,
            atoms.text_uri_list,
            atoms.transfer_prop,
            atoms.incr,
        ) {
            primary_bytes = uri_data.clone();
            fetched_targets.insert("text/uri-list".to_string(), uri_data.clone());
            let text_content = String::from_utf8_lossy(&uri_data);
            let paths: Vec<String> = text_content
                .lines()
                .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
                .map(uri_to_path)
                .collect();

            if !paths.is_empty() {
                uri_paths_vec = Some(paths);
            }
        }
        if has_gnome
            && let Some(gnome_data) = convert_and_fetch_target(
                conn,
                helper_window,
                atoms.clipboard,
                atoms.gnome_copied_files,
                atoms.transfer_prop,
                atoms.incr,
            )
        {
            fetched_targets.insert("x-special/gnome-copied-files".to_string(), gnome_data);
        }
    } else if has_html {
        kind = ItemKind::RichText;
        is_rich = true;
        if let Some(html_data) = convert_and_fetch_target(
            conn,
            helper_window,
            atoms.clipboard,
            atoms.text_html,
            atoms.transfer_prop,
            atoms.incr,
        ) {
            primary_bytes = html_data.clone();
            fetched_targets.insert("text/html".to_string(), html_data);
        }
    }

    if has_utf8 || has_string {
        let (text_atom, text_name) = if has_utf8 {
            (atoms.utf8_string, "UTF8_STRING")
        } else {
            (atoms.string, "STRING")
        };
        if let Some(txt_data) = convert_and_fetch_target(
            conn,
            helper_window,
            atoms.clipboard,
            text_atom,
            atoms.transfer_prop,
            atoms.incr,
        ) {
            fetched_targets.insert(text_name.to_string(), txt_data.clone());
            if let Ok(s) = String::from_utf8(txt_data.clone()) {
                if kind == ItemKind::PlainText {
                    primary_bytes = txt_data;
                }
                plain_text_str = Some(s);
            }
        }
    }

    if primary_bytes.is_empty() && plain_text_str.is_none() {
        return None;
    }

    Some(RawClipboardPayload {
        kind,
        primary_bytes,
        fetched_targets,
        thumbnail,
        image_dimensions,
        plain_text_str,
        uri_paths_vec,
        is_rich,
    })
}

fn handle_new_clipboard_selection(
    conn: &RustConnection,
    atoms: &Atoms,
    helper_window: Window,
    state: &Arc<Mutex<SharedClipboardState>>,
    next_id: &mut u64,
) {
    let Some(mime_names) = fetch_available_mime_names(conn, atoms, helper_window) else {
        return;
    };

    let Some(payload) = fetch_clipboard_payload(conn, atoms, helper_window, &mime_names) else {
        return;
    };

    let total_bytes: usize = payload.fetched_targets.values().map(|v| v.len()).sum();
    let disk_stats = Arc::new(Mutex::new(None));
    let is_password = mime_names.iter().any(|m| is_password_hint(m));

    let item = HistoryItem {
        id: *next_id,
        kind: payload.kind,
        byte_size: total_bytes,
        mime_types: mime_names,
        thumbnail: payload.thumbnail,
        image_dimensions: payload.image_dimensions,
        plain_text: payload.plain_text_str,
        uri_paths: payload.uri_paths_vec,
        is_rich_text: payload.is_rich,
        is_password,
        targets: payload.fetched_targets,
        raw_primary_payload: payload.primary_bytes,
        disk_stats: Arc::clone(&disk_stats),
    };
    *next_id += 1;

    let mut st = state.lock().unwrap();
    if let Some(first) = st.history.first()
        && first.kind == item.kind
        && first.raw_primary_payload == item.raw_primary_payload
    {
        return;
    }

    if let Some(ref paths) = item.uri_paths {
        let disk_stats_clone = Arc::clone(&disk_stats);
        let paths_clone = paths.clone();
        let state_clone = Arc::clone(state);
        std::thread::spawn(move || {
            let stats = compute_disk_stats(&paths_clone);
            if let Ok(mut lock) = disk_stats_clone.lock() {
                *lock = Some(stats);
            }
            if let Ok(mut st) = state_clone.lock() {
                st.notify_subscribers();
            }
        });
    }

    st.last_copied_id = Some(item.id);
    st.history.insert(0, item);
    if st.history.len() > MAX_HISTORY_ITEMS {
        st.history.truncate(MAX_HISTORY_ITEMS);
    }
    st.notify_subscribers();
}
