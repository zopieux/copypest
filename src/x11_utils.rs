use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, Property, Window};
use x11rb::rust_connection::RustConnection;

pub struct Atoms {
    pub clipboard: Atom,
    pub targets: Atom,
    pub utf8_string: Atom,
    pub string: Atom,
    pub text: Atom,
    pub text_plain: Atom,
    pub text_plain_utf8: Atom,
    pub text_uri_list: Atom,
    pub gnome_copied_files: Atom,
    pub text_html: Atom,
    pub image_png: Atom,
    pub image_bmp: Atom,
    pub image_jpeg: Atom,
    pub image_webp: Atom,
    pub transfer_prop: Atom,
    pub incr: Atom,
}

pub fn intern_all_atoms(conn: &RustConnection) -> Result<Atoms, Box<dyn std::error::Error>> {
    let clipboard = conn.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
    let targets = conn.intern_atom(false, b"TARGETS")?.reply()?.atom;
    let utf8_string = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
    let string = conn.intern_atom(false, b"STRING")?.reply()?.atom;
    let text = conn.intern_atom(false, b"TEXT")?.reply()?.atom;
    let text_plain = conn.intern_atom(false, b"text/plain")?.reply()?.atom;
    let text_plain_utf8 = conn
        .intern_atom(false, b"text/plain;charset=utf-8")?
        .reply()?
        .atom;
    let text_uri_list = conn.intern_atom(false, b"text/uri-list")?.reply()?.atom;
    let gnome_copied_files = conn
        .intern_atom(false, b"x-special/gnome-copied-files")?
        .reply()?
        .atom;
    let text_html = conn.intern_atom(false, b"text/html")?.reply()?.atom;
    let image_png = conn.intern_atom(false, b"image/png")?.reply()?.atom;
    let image_bmp = conn.intern_atom(false, b"image/bmp")?.reply()?.atom;
    let image_jpeg = conn.intern_atom(false, b"image/jpeg")?.reply()?.atom;
    let image_webp = conn.intern_atom(false, b"image/webp")?.reply()?.atom;
    let transfer_prop = conn.intern_atom(false, b"COPYPEST_TRANSFER")?.reply()?.atom;
    let incr = conn.intern_atom(false, b"INCR")?.reply()?.atom;

    Ok(Atoms {
        clipboard,
        targets,
        utf8_string,
        string,
        text,
        text_plain,
        text_plain_utf8,
        text_uri_list,
        gnome_copied_files,
        text_html,
        image_png,
        image_bmp,
        image_jpeg,
        image_webp,
        transfer_prop,
        incr,
    })
}

fn fetch_incr_payload(
    conn: &RustConnection,
    window: Window,
    prop: Atom,
    initial_value: &[u8],
) -> Option<Vec<u8>> {
    let _ = conn.flush();
    let lower_bound = if initial_value.len() >= 4 {
        u32::from_ne_bytes(initial_value[0..4].try_into().unwrap()) as usize
    } else {
        0
    };

    let mut buffer = Vec::with_capacity(lower_bound);
    let chunk_timeout = Duration::from_millis(3000);
    let mut last_activity = Instant::now();

    while last_activity.elapsed() < chunk_timeout {
        match conn.poll_for_event() {
            Ok(Some(Event::PropertyNotify(pn))) => {
                if pn.window == window && pn.atom == prop && pn.state == Property::NEW_VALUE {
                    last_activity = Instant::now();
                    let chunk_reply = conn
                        .get_property(true, window, prop, AtomEnum::NONE, 0, 1024 * 1024 * 16)
                        .ok()?
                        .reply()
                        .ok()?;

                    let _ = conn.flush();
                    if chunk_reply.value.is_empty() {
                        return Some(buffer);
                    }
                    buffer.extend_from_slice(&chunk_reply.value);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(_) => return None,
        }
    }

    None
}

pub fn convert_and_fetch_target(
    conn: &RustConnection,
    window: Window,
    clipboard: Atom,
    target: Atom,
    prop: Atom,
    incr: Atom,
) -> Option<Vec<u8>> {
    if conn
        .convert_selection(window, clipboard, target, prop, x11rb::CURRENT_TIME)
        .is_err()
    {
        return None;
    }
    if conn.flush().is_err() {
        return None;
    }

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match conn.poll_for_event() {
            Ok(Some(Event::SelectionNotify(ev))) => {
                if ev.requestor == window && ev.selection == clipboard && ev.target == target {
                    if ev.property == 0 {
                        return None;
                    }
                    let prop_reply = conn
                        .get_property(
                            true,
                            window,
                            ev.property,
                            AtomEnum::NONE,
                            0,
                            1024 * 1024 * 16,
                        )
                        .ok()?
                        .reply()
                        .ok()?;

                    if prop_reply.type_ == incr {
                        return fetch_incr_payload(conn, window, prop, &prop_reply.value);
                    }

                    return Some(prop_reply.value);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => return None,
        }
    }
    None
}

pub struct ServedData {
    pub target_atoms: Vec<Atom>,
    pub payloads: HashMap<Atom, (Atom, Vec<u8>)>,
}
