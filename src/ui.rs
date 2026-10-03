use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, CornerRadius, FontId, Key, Rect, RichText, Sense, Stroke, Vec2};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use crate::format_utils::format_bytes;
use crate::ipc::{IpcRequest, send_ipc_request};
use crate::model::{HistoryItem, ItemKind, Thumbnail, is_password_hint};
use crate::path_utils::{ellide_home, ellide_path_with_measurer};

fn measure_text_width(ui: &egui::Ui, text: &str, font_id: &FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_string(), font_id.clone(), Color32::WHITE)
        .size()
        .x
}

pub fn ellide_path_to_width(path_str: &str, max_width: f32, ui: &egui::Ui) -> String {
    let font_id = FontId::proportional(13.0);
    ellide_path_with_measurer(path_str, max_width, |t| measure_text_width(ui, t, &font_id))
}

fn collapse_whitespace(text: &str) -> String {
    let collapsed: String = text
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    collapsed.trim().to_string()
}

pub fn format_item_title(item: &HistoryItem) -> String {
    match item.kind {
        ItemKind::PlainText | ItemKind::RichText => {
            if let Some(ref pt) = item.plain_text {
                let s = collapse_whitespace(pt);
                if s.is_empty() {
                    "Empty text".to_string()
                } else {
                    s
                }
            } else {
                "Text".to_string()
            }
        }
        ItemKind::UriList => {
            if let Some(ref paths) = item.uri_paths {
                if paths.is_empty() {
                    "Files".to_string()
                } else {
                    let first = ellide_home(&paths[0]);
                    if paths.len() == 1 {
                        first
                    } else {
                        format!("{} (+{} files)", first, paths.len() - 1)
                    }
                }
            } else {
                "Files".to_string()
            }
        }
        ItemKind::Image => {
            let img_type = item
                .mime_types
                .iter()
                .find(|m| m.starts_with("image/"))
                .and_then(|m| m.strip_prefix("image/"))
                .unwrap_or("Image")
                .to_uppercase();
            if let Some((w, h)) = item.image_dimensions {
                format!("{img_type} Image ({w}×{h})")
            } else {
                format!("{img_type} Image")
            }
        }
    }
}

pub fn format_item_title_for_ui(item: &HistoryItem, max_width: f32, ui: &egui::Ui) -> String {
    match item.kind {
        ItemKind::PlainText | ItemKind::RichText => {
            if let Some(ref pt) = item.plain_text {
                let s = collapse_whitespace(pt);
                if s.is_empty() {
                    "Empty text".to_string()
                } else {
                    s
                }
            } else {
                "Text".to_string()
            }
        }
        ItemKind::UriList => {
            if let Some(ref paths) = item.uri_paths {
                if paths.is_empty() {
                    "Files".to_string()
                } else if paths.len() == 1 {
                    ellide_path_to_width(&paths[0], max_width, ui)
                } else {
                    let suffix = format!(" (+{} files)", paths.len() - 1);
                    let font_id = FontId::proportional(13.0);
                    let suffix_w = measure_text_width(ui, &suffix, &font_id);
                    let path_budget = (max_width - suffix_w).max(20.0);
                    let collapsed = ellide_path_to_width(&paths[0], path_budget, ui);
                    format!("{collapsed}{suffix}")
                }
            } else {
                "Files".to_string()
            }
        }
        ItemKind::Image => {
            let img_type = item
                .mime_types
                .iter()
                .find(|m| m.starts_with("image/"))
                .and_then(|m| m.strip_prefix("image/"))
                .unwrap_or("Image")
                .to_uppercase();
            if let Some((w, h)) = item.image_dimensions {
                format!("{img_type} Image ({w}×{h})")
            } else {
                format!("{img_type} Image")
            }
        }
    }
}

fn filter_and_score_items(
    items: &[HistoryItem],
    query: &str,
    matcher: &SkimMatcherV2,
) -> Vec<(i64, HistoryItem)> {
    let query = query.trim();
    if query.is_empty() {
        return items.iter().map(|it| (0, it.clone())).collect();
    }

    let mut filtered_items = Vec::new();
    for item in items {
        let title = format_item_title(item);
        let mut score = matcher.fuzzy_match(&title, query);
        if score.is_none()
            && let Some(ref pt) = item.plain_text
        {
            score = matcher.fuzzy_match(pt, query);
        }
        if score.is_none()
            && let Some(ref paths) = item.uri_paths
        {
            for p in paths {
                if let Some(s) = matcher.fuzzy_match(p, query) {
                    score = Some(s);
                    break;
                }
            }
        }
        if score.is_none() {
            for m in &item.mime_types {
                if let Some(s) = matcher.fuzzy_match(m, query) {
                    score = Some(s);
                    break;
                }
            }
        }
        if score.is_none() && item.is_password {
            score = matcher.fuzzy_match("password", query);
        }
        if let Some(s) = score {
            filtered_items.push((s, item.clone()));
        }
    }
    filtered_items.sort_by_key(|b| std::cmp::Reverse(b.0));
    filtered_items
}

// --- UI Free Functions ---

fn render_top_panel(
    ui: &mut egui::Ui,
    search_query: &mut String,
    focus_search: &mut bool,
) -> (bool, f32) {
    let top_frame = egui::Frame::default()
        .fill(Color32::from_rgb(20, 22, 32))
        .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 10,
            bottom: 10,
        })
        .stroke(Stroke::NONE);

    let mut query_changed = false;
    let top_res = egui::Panel::top("top_search_panel")
        .frame(top_frame)
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(2.0);
                ui.label(
                    RichText::new("🔍")
                        .size(15.0)
                        .color(Color32::from_rgb(130, 145, 175)),
                );
                ui.add_space(8.0);
                let text_edit = egui::TextEdit::singleline(search_query)
                    .hint_text("Type to filter clipboard...")
                    .font(FontId::proportional(14.5))
                    .text_color(Color32::from_rgb(235, 240, 252))
                    .frame(egui::Frame::NONE)
                    .desired_width(ui.available_width());

                let response = ui.add(text_edit);
                if *focus_search {
                    response.request_focus();
                    *focus_search = false;
                }
                if response.changed() {
                    query_changed = true;
                }
            });
        });

    (query_changed, top_res.response.rect.max.y)
}

fn render_action_shortcut(ui: &mut egui::Ui, key: &str, desc: &str, highlight: bool) {
    let k_color = if highlight {
        Color32::from_rgb(130, 205, 255)
    } else {
        Color32::from_rgb(175, 185, 205)
    };
    let d_color = if highlight {
        Color32::from_rgb(220, 235, 255)
    } else {
        Color32::from_rgb(130, 140, 165)
    };

    ui.add(
        egui::Label::new(RichText::new(key).size(11.0).strong().color(k_color)).selectable(false),
    );
    ui.add_space(5.0);
    ui.add(egui::Label::new(RichText::new(desc).size(11.0).color(d_color)).selectable(false));
    ui.add_space(12.0);
}

fn render_bottom_panel(ui: &mut egui::Ui, selected_item: Option<&HistoryItem>) {
    let bottom_frame = egui::Frame::default()
        .fill(Color32::from_rgb(18, 20, 28))
        .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 8,
            bottom: 8,
        })
        .stroke(Stroke::NONE);

    egui::Panel::bottom("bottom_action_panel")
        .frame(bottom_frame)
        .show_separator_line(true)
        .show(ui, |ui| {
            ui.style_mut().interaction.selectable_labels = false;
            ui.horizontal_centered(|ui| {
                render_action_shortcut(ui, "<Ctrl+C>", "Copy", false);

                if let Some(it) = selected_item {
                    if it.is_rich_text {
                        render_action_shortcut(ui, "<Ctrl+Shift+C>", "Copy plain text", false);
                    } else if it.kind == ItemKind::UriList {
                        render_action_shortcut(ui, "<Ctrl+Shift+C>", "Copy absolute path", false);
                    }
                }

                render_action_shortcut(ui, "<Delete>", "Remove", false);
                render_action_shortcut(ui, "<Esc>", "Close", false);
            });
        });
}

fn render_empty_state(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(
            RichText::new("Clipboard history is empty")
                .color(Color32::from_rgb(120, 130, 150))
                .size(13.0),
        );
    });
}

fn render_badge(ui: &mut egui::Ui, rect: Rect, kind: ItemKind) {
    let (badge_text, badge_color) = match kind {
        ItemKind::PlainText => ("TXT", Color32::from_rgb(85, 145, 235)),
        ItemKind::Image => ("IMG", Color32::from_rgb(230, 145, 65)),
        ItemKind::UriList => ("FILE", Color32::from_rgb(70, 190, 125)),
        ItemKind::RichText => ("RICH", Color32::from_rgb(190, 105, 225)),
    };

    let mut badge_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let badge_frame = egui::Frame::default()
        .fill(badge_color.linear_multiply(0.18))
        .stroke(Stroke::new(1.0, badge_color.linear_multiply(0.85)))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::ZERO);

    badge_frame.show(&mut badge_ui, |ui| {
        ui.allocate_ui_with_layout(
            rect.size(),
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
            |ui| {
                ui.label(
                    RichText::new(badge_text)
                        .size(10.5)
                        .strong()
                        .color(badge_color),
                );
            },
        );
    });
}

fn render_title_line(ui: &mut egui::Ui, title: &str, is_selected: bool) {
    ui.horizontal(|ui| {
        let text_color = if is_selected {
            Color32::from_rgb(255, 255, 255)
        } else {
            Color32::from_rgb(215, 220, 235)
        };
        ui.add(egui::Label::new(RichText::new(title).size(13.0).color(text_color)).truncate());
    });
}

fn render_meta_line(ui: &mut egui::Ui, item: &HistoryItem, ctx: &egui::Context) {
    let mut text_line_count = None;
    if (item.kind == ItemKind::PlainText
        || item.kind == ItemKind::RichText
        || item
            .mime_types
            .iter()
            .any(|m| m == "text/plain" || m == "text/html"))
        && let Some(ref pt) = item.plain_text
    {
        text_line_count = Some(if pt.is_empty() {
            0
        } else {
            pt.lines().count().max(1)
        });
    }

    let size_str = if item.kind == ItemKind::UriList {
        let stats_opt = *item.disk_stats.lock().unwrap();
        if let Some(ref stats) = stats_opt {
            stats.format_report()
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            "computing size…".to_string()
        }
    } else if let Some(lines) = text_line_count {
        let line_label = if lines == 1 {
            "1 line".to_string()
        } else {
            format!("{lines} lines")
        };
        format!("{} • {line_label}", format_bytes(item.byte_size))
    } else {
        format_bytes(item.byte_size)
    };

    let mut mimes = Vec::new();
    let mut specials = Vec::new();

    for m in &item.mime_types {
        if is_password_hint(m) {
            continue;
        }
        if m.contains('/') {
            mimes.push(m.as_str());
        } else {
            specials.push(m.as_str());
        }
    }

    mimes.sort_unstable();
    specials.sort_unstable();

    let font_id = FontId::proportional(10.5);
    let base_height = ui
        .painter()
        .layout_no_wrap("M".to_string(), font_id.clone(), Color32::WHITE)
        .size()
        .y;
    let line_height = Some(base_height + 1.0);
    let mut job = LayoutJob::default();

    if item.is_password {
        job.append(
            "⚠ Password",
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: Color32::from_rgb(250, 180, 50),
                line_height,
                ..Default::default()
            },
        );
        job.append(
            " • ",
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: Color32::from_rgb(70, 80, 105),
                line_height,
                ..Default::default()
            },
        );
    }

    job.append(
        &size_str,
        0.0,
        TextFormat {
            font_id: font_id.clone(),
            color: Color32::from_rgb(135, 160, 205),
            line_height,
            ..Default::default()
        },
    );

    if !mimes.is_empty() {
        job.append(
            " • ",
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: Color32::from_rgb(70, 80, 105),
                line_height,
                ..Default::default()
            },
        );
        let mimes_str = mimes.join(", ");
        job.append(
            &mimes_str,
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: Color32::from_rgb(155, 165, 190),
                line_height,
                ..Default::default()
            },
        );
    }

    if !specials.is_empty() {
        job.append(
            " • ",
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: Color32::from_rgb(70, 80, 105),
                line_height,
                ..Default::default()
            },
        );
        let specials_str = specials.join(", ");
        job.append(
            &specials_str,
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: Color32::from_rgb(95, 105, 135),
                line_height,
                ..Default::default()
            },
        );
    }

    job.wrap.max_width = ui.available_width();
    ui.label(job);
}

fn render_thumbnail(
    ui: &mut egui::Ui,
    rect: Rect,
    item_id: u64,
    thumb: &Thumbnail,
    texture_cache: &mut HashMap<u64, egui::TextureHandle>,
    ctx: &egui::Context,
) {
    let tex = texture_cache.entry(item_id).or_insert_with(|| {
        let img =
            egui::ColorImage::from_rgba_unmultiplied([thumb.width, thumb.height], &thumb.rgba);
        ctx.load_texture(
            format!("thumb_{item_id}"),
            img,
            egui::TextureOptions::LINEAR,
        )
    });

    let mut thumb_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    thumb_ui.image((tex.id(), rect.size()));
}

struct ItemRowContext<'a> {
    idx: usize,
    is_selected: bool,
    scroll_to_selected: bool,
    texture_cache: &'a mut HashMap<u64, egui::TextureHandle>,
    ctx: &'a egui::Context,
}

fn render_history_item_row(
    ui: &mut egui::Ui,
    item: &HistoryItem,
    cx: &mut ItemRowContext<'_>,
) -> bool {
    let bg_color = if cx.is_selected {
        Color32::from_rgb(32, 40, 62)
    } else {
        Color32::TRANSPARENT
    };

    let start_pos = ui.cursor().min;
    let avail_width = ui.available_width();

    let left_pad = 14.0;
    let right_pad = 14.0;
    let h_gap = 12.0;
    let badge_w = 46.0;
    let badge_h = 20.0;
    let top_pad = 9.0;
    let bottom_pad = 9.0;

    let (has_thumb, thumb_w, thumb_h) = if let Some(ref thumb) = item.thumbnail {
        let orig_w = thumb.width as f32;
        let orig_h = thumb.height as f32;
        let max_w: f32 = 48.0;
        let max_h: f32 = 36.0;
        let scale = 1.0f32
            .min(max_w / orig_w.max(1.0))
            .min(max_h / orig_h.max(1.0));
        let w = (orig_w * scale).max(1.0);
        let h = (orig_h * scale).max(1.0);
        (true, w, h)
    } else {
        (false, 0.0, 0.0)
    };

    let text_w = if has_thumb {
        avail_width - left_pad - badge_w - h_gap - h_gap - thumb_w - right_pad
    } else {
        avail_width - left_pad - badge_w - h_gap - right_pad
    }
    .max(80.0);

    // Reserve background and indicator shapes before child UIs so they render behind
    let bg_shape_idx = ui.painter().add(egui::Shape::Noop);
    let ind_shape_idx = ui.painter().add(egui::Shape::Noop);
    let sep_shape_idx = ui.painter().add(egui::Shape::Noop);

    // 1. Center text block
    let text_x = start_pos.x + left_pad + badge_w + h_gap;
    let text_y = start_pos.y + top_pad;
    let text_rect_max = Rect::from_min_size(egui::pos2(text_x, text_y), Vec2::new(text_w, 2000.0));

    let mut text_ui = ui.new_child(egui::UiBuilder::new().max_rect(text_rect_max));
    text_ui.spacing_mut().item_spacing = Vec2::new(0.0, 3.0);

    // Line 1: title
    let title = format_item_title_for_ui(item, text_w, &text_ui);
    render_title_line(&mut text_ui, &title, cx.is_selected);

    // Line 2: size, MIME types, and special protocol words
    render_meta_line(&mut text_ui, item, cx.ctx);

    let text_rect = text_ui.min_rect();
    let text_h = text_rect.height();
    let content_h = text_h.max(badge_h).max(thumb_h);
    let row_height = top_pad + content_h + bottom_pad;
    let row_rect = Rect::from_min_size(start_pos, Vec2::new(avail_width, row_height));

    // 2. Left Badge box: exact fixed width 46px
    let badge_rect = Rect::from_min_size(
        egui::pos2(start_pos.x + left_pad, start_pos.y + top_pad),
        Vec2::new(badge_w, badge_h),
    );
    render_badge(ui, badge_rect, item.kind);

    // 3. Right Thumbnail: centered vertically
    if has_thumb && let Some(ref thumb) = item.thumbnail {
        let thumb_x = start_pos.x + avail_width - right_pad - thumb_w;
        let thumb_y = start_pos.y + (row_height - thumb_h) / 2.0;
        let thumb_rect =
            Rect::from_min_size(egui::pos2(thumb_x, thumb_y), Vec2::new(thumb_w, thumb_h));
        render_thumbnail(ui, thumb_rect, item.id, thumb, cx.texture_cache, cx.ctx);
    }

    // Set background & accents
    let bg_rect = if cx.idx == 0 {
        Rect::from_min_max(
            egui::pos2(row_rect.min.x, row_rect.min.y - 1.0),
            row_rect.max,
        )
    } else {
        row_rect
    };

    if bg_color != Color32::TRANSPARENT {
        ui.painter().set(
            bg_shape_idx,
            egui::Shape::rect_filled(bg_rect, CornerRadius::ZERO, bg_color),
        );
    }

    let ind_min_y = if cx.idx == 0 {
        row_rect.min.y - 1.0
    } else {
        row_rect.min.y
    };
    let ind = Rect::from_min_max(
        egui::pos2(row_rect.min.x, ind_min_y),
        egui::pos2(row_rect.min.x + 3.0, row_rect.max.y),
    );

    if cx.is_selected {
        ui.painter().set(
            ind_shape_idx,
            egui::Shape::rect_filled(ind, CornerRadius::ZERO, Color32::from_rgb(75, 135, 235)),
        );
    }

    let sep_y = row_rect.max.y;
    ui.painter().set(
        sep_shape_idx,
        egui::Shape::line_segment(
            [
                egui::pos2(row_rect.min.x, sep_y),
                egui::pos2(row_rect.max.x, sep_y),
            ],
            Stroke::new(1.0, Color32::from_rgb(35, 40, 58)),
        ),
    );

    let row_interact = ui.interact(
        row_rect,
        egui::Id::new(("row_item", item.id)),
        Sense::click(),
    );
    let clicked = row_interact.clicked();

    if cx.is_selected && cx.scroll_to_selected {
        ui.scroll_to_rect(row_rect, None);
    }

    ui.advance_cursor_after_rect(row_rect);

    clicked
}

// --- App State ---

pub struct CopypestApp {
    items: Vec<HistoryItem>,
    update_rx: Receiver<Vec<HistoryItem>>,
    search_query: String,
    selected_index: usize,
    focus_search: bool,
    scroll_to_selected: bool,
    texture_cache: HashMap<u64, egui::TextureHandle>,
    matcher: SkimMatcherV2,
}

impl CopypestApp {
    pub fn new(items: Vec<HistoryItem>, update_rx: Receiver<Vec<HistoryItem>>) -> Self {
        Self {
            items,
            update_rx,
            search_query: String::new(),
            selected_index: 0,
            focus_search: true,
            scroll_to_selected: false,
            texture_cache: HashMap::new(),
            matcher: SkimMatcherV2::default(),
        }
    }

    fn handle_incoming_updates(&mut self) {
        let mut updated = false;
        while let Ok(latest) = self.update_rx.try_recv() {
            self.items = latest;
            updated = true;
        }
        if updated {
            self.texture_cache
                .retain(|id, _| self.items.iter().any(|it| it.id == *id));
        }
    }
}

impl eframe::App for CopypestApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ui.spacing_mut().item_spacing = Vec2::ZERO;

        self.handle_incoming_updates();

        let filtered_items = filter_and_score_items(&self.items, &self.search_query, &self.matcher);
        let total_items = filtered_items.len();
        if total_items == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= total_items {
            self.selected_index = total_items - 1;
        }

        let selected_item: Option<HistoryItem> = filtered_items
            .get(self.selected_index)
            .map(|(_, it)| it.clone());

        // Keyboard shortcuts
        let mut ctrl_c = false;
        let mut ctrl_shift_c = false;

        ui.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Copy => {
                        if i.modifiers.shift {
                            ctrl_shift_c = true;
                        } else {
                            ctrl_c = true;
                        }
                    }
                    egui::Event::Key {
                        key,
                        physical_key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        let is_c = *key == Key::C || *physical_key == Some(Key::C);
                        if is_c && (modifiers.ctrl || modifiers.command) {
                            if modifiers.shift {
                                ctrl_shift_c = true;
                            } else {
                                ctrl_c = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !ctrl_c
                && !ctrl_shift_c
                && (i.modifiers.ctrl || i.modifiers.command)
                && i.key_pressed(Key::C)
            {
                if i.modifiers.shift {
                    ctrl_shift_c = true;
                } else {
                    ctrl_c = true;
                }
            }
        });

        let key_up = ui.input(|i| i.key_pressed(Key::ArrowUp));
        let key_down = ui.input(|i| i.key_pressed(Key::ArrowDown));
        let key_page_up = ui.input(|i| i.key_pressed(Key::PageUp));
        let key_page_down = ui.input(|i| i.key_pressed(Key::PageDown));
        let key_home = ui.input(|i| i.key_pressed(Key::Home));
        let key_end = ui.input(|i| i.key_pressed(Key::End));
        let key_escape = ui.input(|i| i.key_pressed(Key::Escape));
        let key_delete = ui.input(|i| i.key_pressed(Key::Delete));

        if key_escape {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if key_up && self.selected_index > 0 {
            self.selected_index -= 1;
            self.scroll_to_selected = true;
        }
        if key_down && total_items > 0 && self.selected_index + 1 < total_items {
            self.selected_index += 1;
            self.scroll_to_selected = true;
        }
        if key_page_up {
            self.selected_index = self.selected_index.saturating_sub(5);
            self.scroll_to_selected = true;
        }
        if key_page_down && total_items > 0 {
            self.selected_index = (self.selected_index + 5).min(total_items - 1);
            self.scroll_to_selected = true;
        }
        if key_home {
            self.selected_index = 0;
            self.scroll_to_selected = true;
        }
        if key_end && total_items > 0 {
            self.selected_index = total_items - 1;
            self.scroll_to_selected = true;
        }

        if key_delete && let Some(ref it) = selected_item {
            let _ = send_ipc_request(&IpcRequest::DeleteItem(it.id));
            self.items.retain(|item| item.id != it.id);
            if self.selected_index >= total_items.saturating_sub(1) {
                self.selected_index = total_items.saturating_sub(2);
            }
        }

        let mut execute_native_copy = false;
        let mut execute_alt_copy = false;

        if (ctrl_c || ctrl_shift_c)
            && let Some(ref it) = selected_item
        {
            if ctrl_shift_c {
                let is_applicable = it.is_rich_text || it.kind == ItemKind::UriList;
                if is_applicable {
                    execute_alt_copy = true;
                } else {
                    execute_native_copy = true;
                }
            } else {
                execute_native_copy = true;
            }
        }

        if execute_native_copy {
            if let Some(ref it) = selected_item {
                let _ = send_ipc_request(&IpcRequest::RestoreNative(it.id));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if execute_alt_copy {
            if let Some(ref it) = selected_item {
                if it.kind == ItemKind::UriList {
                    let _ = send_ipc_request(&IpcRequest::RestorePathText(it.id));
                } else if it.is_rich_text {
                    let _ = send_ipc_request(&IpcRequest::RestorePlainText(it.id));
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // --- 1. Top Panel: Search Bar ---
        let (query_changed, top_sep_y) =
            render_top_panel(ui, &mut self.search_query, &mut self.focus_search);
        if query_changed {
            self.selected_index = 0;
            self.scroll_to_selected = true;
        }

        // --- 2. Bottom Panel: Live Actions ---
        render_bottom_panel(ui, selected_item.as_ref());

        // --- 3. Central Panel: Full-Width Rows ---
        let center_frame = egui::Frame::default()
            .fill(Color32::from_rgb(22, 25, 36))
            .inner_margin(egui::Margin::ZERO)
            .stroke(Stroke::NONE);

        egui::CentralPanel::default()
            .frame(center_frame)
            .show(ui, |ui| {
                let mut clicked_row: Option<usize> = None;

                // Separator line under searchbar
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    top_sep_y,
                    Stroke::new(1.0, Color32::from_rgb(35, 40, 58)),
                );

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;

                        if filtered_items.is_empty() {
                            render_empty_state(ui);
                            return;
                        }

                        for (idx, (_, item)) in filtered_items.iter().enumerate() {
                            let is_selected = idx == self.selected_index;

                            let mut row_cx = ItemRowContext {
                                idx,
                                is_selected,
                                scroll_to_selected: self.scroll_to_selected,
                                texture_cache: &mut self.texture_cache,
                                ctx: &ctx,
                            };
                            if render_history_item_row(ui, item, &mut row_cx) {
                                clicked_row = Some(idx);
                            }
                        }

                        self.scroll_to_selected = false;
                    });

                // Mouse click on row updates selection only (no copy)
                if let Some(idx) = clicked_row {
                    self.selected_index = idx;
                }
            });
    }
}
