//! Home screen: every page drawn from SpringBoard's icon layout, plus "tidy" (A→Z) with a backup and restore.

use crate::{card, fg, muted, Job, ACCENT, BAD, GOOD, WARN};
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke};
use idevice::springboardservices::SpringBoardServicesClient;
use plist::Value;
use std::collections::HashMap;

/// Icons per home page on this phone class (4 x 6).
const PER_PAGE: usize = 24;

pub struct HomeData { pub pages: Vec<Value>, pub icons: HashMap<String, egui::ColorImage> }

fn d(v: &Value) -> Option<&plist::Dictionary> { v.as_dictionary() }
pub fn item_id(v: &Value) -> String {
    d(v).and_then(|d| ["bundleIdentifier", "displayIdentifier", "containerBundleIdentifier"].iter().find_map(|k| d.get(*k).and_then(|v| v.as_string())))
        .unwrap_or_default().to_string()
}
fn name(v: &Value) -> String { d(v).and_then(|d| d.get("displayName")).and_then(|v| v.as_string()).unwrap_or("").trim_start_matches('\u{200e}').to_string() }
fn is_widget(v: &Value) -> bool { d(v).is_some_and(|d| d.contains_key("gridSize") || d.contains_key("iconType")) }
fn folder_items(v: &Value) -> Option<Vec<Value>> {
    d(v)?.get("iconLists")?.as_array()?.first()?.as_array().cloned()
}
/// Cells a widget spans.
fn span(v: &Value) -> (usize, usize) {
    match d(v).and_then(|d| d.get("gridSize")).and_then(|v| v.as_string()) {
        Some("medium") => (4, 2), Some("large") | Some("extraLarge") => (4, 4), Some(_) => (2, 2), None => (1, 1),
    }
}

pub async fn fetch() -> Result<HomeData, String> {
    let mut sb = crate::device::service::<SpringBoardServicesClient>().await?;
    let state = sb.get_icon_state(Some("2")).await.map_err(|e| format!("getIconState: {e}"))?;
    let pages = state.as_array().cloned().ok_or("icon state is not a list")?;
    let mut ids: Vec<String> = Vec::new();
    for p in &pages {
        for it in p.as_array().into_iter().flatten() {
            ids.push(item_id(it));
            for f in folder_items(it).into_iter().flatten().take(9) { ids.push(item_id(&f)); }
        }
    }
    ids.sort();
    ids.dedup();
    let mut icons = HashMap::new();
    for id in ids.into_iter().filter(|s| !s.is_empty()) {
        if let Some(img) = sb.get_icon_pngdata(id.clone()).await.ok().and_then(|png| crate::decode(png, 96)) { icons.insert(id, img); }
    }
    Ok(HomeData { pages, icons })
}

/// A→Z within pages that hold only apps and folders (folders first, kept in order). The dock, page 1 and any page
/// with a widget stay exactly as they are; sorted items refill the app-only pages' own positions.
pub fn tidy(pages: &[Value]) -> Vec<Value> {
    let movable: Vec<usize> = (2..pages.len()).filter(|&k| !pages[k].as_array().is_some_and(|a| a.iter().any(is_widget))).collect();
    let mut pool: Vec<Value> = movable.iter().flat_map(|&k| pages[k].as_array().cloned().unwrap_or_default()).collect();
    pool.sort_by_key(|v| (folder_items(v).is_none(), name(v).to_lowercase()));
    let mut chunks = pool.chunks(PER_PAGE).map(|c| Value::Array(c.to_vec()));
    let mut out = Vec::new();
    for (k, p) in pages.iter().enumerate() {
        if movable.contains(&k) { if let Some(c) = chunks.next() { out.push(c); } } else { out.push(p.clone()); }
    }
    out
}

fn backups() -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(crate::data_dir()).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("homescreen-"))).collect();
    v.sort();
    v
}

async fn apply(pages: Vec<Value>, backup_of: Option<Vec<Value>>) -> Result<String, String> {
    if let Some(old) = backup_of {
        let path = crate::data_dir().join(format!("homescreen-{}.plist", crate::stamp()));
        Value::Array(old).to_file_xml(&path).map_err(|e| format!("backup failed, nothing changed: {e}"))?;
    }
    let mut sb = crate::device::service::<SpringBoardServicesClient>().await?;
    sb.set_icon_state_with_version(Value::Array(pages), Some("2")).await.map_err(|e| format!("setIconState: {e}"))?;
    Ok("Home screen updated.".into())
}

#[derive(Default)]
pub struct HomePage {
    tex: HashMap<String, egui::TextureHandle>,
    loaded: bool,
    preview: Option<Vec<Value>>,
    confirm: Option<(Vec<Value>, bool)>, // (layout, take backup first)
    write: Job<String>,
}

impl HomePage {
    pub fn ui(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Runtime, data: &mut Job<HomeData>) {
        self.write.poll();
        if let Some(Ok(_)) = self.write.value() { if !self.write.running() && self.preview.is_some() { self.preview = None; data.clear(); self.loaded = false; } }
        let Some(res) = data.value() else { card(ui, |ui| ui.label(RichText::new("Loading home screen layout and icons…").color(muted()))); return; };
        let h = match res { Ok(h) => h, Err(e) => { card(ui, |ui| ui.colored_label(BAD, e)); return; } };
        if !self.loaded {
            self.tex = h.icons.iter().map(|(k, img)| (k.clone(), ui.ctx().load_texture(format!("icon:{k}"), img.clone(), egui::TextureOptions::LINEAR))).collect();
            self.loaded = true;
        }

        card(ui, |ui| {
            let apps: usize = h.pages.iter().skip(1).flat_map(|p| p.as_array().cloned().unwrap_or_default()).filter(|v| !is_widget(v) && folder_items(v).is_none()).count();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} pages  ·  {} apps on them  ·  {} in the dock", h.pages.len().saturating_sub(1), apps, h.pages.first().and_then(|p| p.as_array()).map_or(0, |a| a.len()))).color(fg()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.preview.is_some() {
                        if ui.button("Cancel").clicked() { self.preview = None; }
                        if ui.add(egui::Button::new(RichText::new("Apply to iPhone").color(Color32::WHITE)).fill(ACCENT)).clicked() { self.confirm = self.preview.clone().map(|p| (p, true)); }
                    } else {
                        if let Some(last) = backups().last().cloned() {
                            if ui.button("Restore last backup").on_hover_text(last.display().to_string()).clicked() {
                                if let Ok(Value::Array(p)) = Value::from_file(&last) { self.confirm = Some((p, false)); }
                            }
                        }
                        if ui.button("Tidy: sort apps A to Z").on_hover_text("Preview first. Page 1, the dock and pages with widgets stay as they are.").clicked() { self.preview = Some(tidy(&h.pages)); }
                    }
                });
            });
            if self.preview.is_some() { ui.label(RichText::new("Preview: nothing has changed on the phone yet.").color(WARN)); }
            match self.write.value() { Some(Ok(m)) => { ui.colored_label(GOOD, m); } Some(Err(e)) => { ui.colored_label(BAD, e); } None => {} }
        });
        ui.add_space(12.0);

        let pages = self.preview.as_ref().unwrap_or(&h.pages);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for (k, p) in pages.iter().enumerate().skip(1) {
                    ui.vertical(|ui| {
                        let dock = pages.first().and_then(|d| d.as_array()).map(Vec::as_slice).unwrap_or(&[]);
                        mini_phone(ui, p.as_array().map(Vec::as_slice).unwrap_or(&[]), dock, &self.tex);
                        ui.label(RichText::new(format!("Page {k}")).size(12.0).color(muted()));
                    });
                    ui.add_space(10.0);
                }
            });
        });

        if let Some((layout, backup)) = self.confirm.clone() {
            egui::Modal::new(egui::Id::new("home_confirm")).show(ui.ctx(), |ui| {
                ui.label(RichText::new(if backup { "Rearrange the iPhone's home screen?" } else { "Restore this home screen backup?" }).size(16.0));
                ui.label(RichText::new(if backup { "The current layout is saved first, so \"Restore last backup\" can undo it." } else { "This replaces the current layout." }).color(muted()));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        let old = backup.then(|| h.pages.clone());
                        self.write.clear();
                        self.write.start(rt, ui.ctx(), apply(layout.clone(), old));
                        self.confirm = None;
                    }
                    if ui.button("Cancel").clicked() { self.confirm = None; }
                });
            });
        }
    }
}

/// One page in a small phone: 4x6 grid with widget spans, labels, dock.
fn mini_phone(ui: &mut egui::Ui, items: &[Value], dock: &[Value], tex: &HashMap<String, egui::TextureHandle>) {
    let (rect, _) = ui.allocate_exact_size(vec2(190.0, 400.0), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(30), Color32::from_rgb(0x3A, 0x3D, 0x44));
    let screen = rect.shrink(6.0);
    let mut mesh = egui::Mesh::default();
    let (a, b) = (Color32::from_rgb(0x1B, 0x22, 0x3A), Color32::from_rgb(0x0B, 0x0D, 0x14));
    for (pt, c) in [(screen.left_top(), a), (screen.right_top(), a), (screen.right_bottom(), b), (screen.left_bottom(), b)] { mesh.colored_vertex(pt, c); }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(mesh));
    p.rect_stroke(screen.expand(3.0), CornerRadius::same(27), Stroke::new(6.0, Color32::from_rgb(0x3A, 0x3D, 0x44)), egui::StrokeKind::Middle);

    let pad = screen.width() * 0.07;
    let cell = (screen.width() - 2.0 * pad) / 4.0;
    let icon = cell * 0.70;
    let row_h = cell * 1.18;
    let top = screen.top() + 30.0;
    let draw_icon = |r: Rect, id: &str| match tex.get(id) {
        Some(t) => { egui::Image::new(egui::load::SizedTexture::from_handle(t)).corner_radius(CornerRadius::same((r.width() * 0.225) as u8)).paint_at(ui, r); }
        None => { p.rect_filled(r, CornerRadius::same((r.width() * 0.225) as u8), Color32::from_white_alpha(40)); }
    };
    let mut used = [[false; 4]; 6];
    for it in items {
        let (w, hh) = span(it);
        let Some((r, c)) = (0..=6 - hh).flat_map(|r| (0..=4 - w).map(move |c| (r, c))).find(|&(r, c)| (r..r + hh).all(|rr| (c..c + w).all(|cc| !used[rr][cc]))) else { continue };
        for rr in r..r + hh { for cc in c..c + w { used[rr][cc] = true; } }
        let x = screen.left() + pad + c as f32 * cell + (cell - icon) / 2.0;
        let y = top + r as f32 * row_h;
        let area = Rect::from_min_size(pos2(x, y), vec2(cell * (w as f32 - 1.0) + icon, row_h * (hh as f32 - 1.0) + icon));
        if is_widget(it) {
            p.rect_filled(area, CornerRadius::same((icon * 0.32) as u8), Color32::from_white_alpha(28));
            draw_icon(Rect::from_center_size(area.center(), vec2(icon, icon)), &item_id(it));
        } else if let Some(fi) = folder_items(it) {
            p.rect_filled(area, CornerRadius::same((icon * 0.225) as u8), Color32::from_white_alpha(45));
            let m = (icon - 4.0) / 3.0;
            for (k, f) in fi.iter().take(9).enumerate() {
                draw_icon(Rect::from_min_size(area.min + vec2(2.0 + (k % 3) as f32 * m, 2.0 + (k / 3) as f32 * m), vec2(m - 1.0, m - 1.0)), &item_id(f));
            }
        } else {
            draw_icon(area, &item_id(it));
        }
        let mut label = name(it);
        if label.chars().count() > 10 { label = format!("{}…", label.chars().take(9).collect::<String>()); }
        p.text(pos2(area.center().x, area.bottom() + 5.5), egui::Align2::CENTER_CENTER, label, FontId::proportional(6.5), Color32::from_white_alpha(220));
    }
    let dock_r = Rect::from_min_max(pos2(screen.left() + 6.0, screen.bottom() - icon - 18.0), pos2(screen.right() - 6.0, screen.bottom() - 6.0));
    p.rect_filled(dock_r, CornerRadius::same(20), Color32::from_white_alpha(30));
    let n = dock.len().max(1) as f32;
    for (k, it) in dock.iter().enumerate() {
        draw_icon(Rect::from_center_size(pos2(dock_r.left() + dock_r.width() * (k as f32 + 0.5) / n, dock_r.center().y), vec2(icon, icon)), &item_id(it));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(n: &str) -> Value { Value::Dictionary(plist::Dictionary::from_iter([("displayName".to_string(), Value::String(n.into())), ("bundleIdentifier".to_string(), Value::String(format!("id.{n}")))])) }
    fn widget() -> Value { Value::Dictionary(plist::Dictionary::from_iter([("gridSize".to_string(), Value::String("small".into()))])) }
    fn folder(n: &str) -> Value { Value::Dictionary(plist::Dictionary::from_iter([("displayName".to_string(), Value::String(n.into())), ("listType".to_string(), Value::String("folder".into())), ("iconLists".to_string(), Value::Array(vec![Value::Array(vec![app("in")])]))])) }
    fn names(p: &Value) -> Vec<String> { p.as_array().unwrap().iter().map(name).collect() }

    #[test]
    fn tidy_keeps_dock_page1_and_widget_pages() {
        let pages = vec![
            Value::Array(vec![app("Phone")]),
            Value::Array(vec![app("Zed"), app("Alpha")]),
            Value::Array(vec![app("mango"), folder("Utils"), app("Banana")]),
            Value::Array(vec![widget(), app("Yak")]),
            Value::Array(vec![app("apple")]),
        ];
        let t = tidy(&pages);
        assert_eq!(t.len(), 4); // two app-only pages pooled into one
        assert_eq!(names(&t[0]), ["Phone"]);
        assert_eq!(names(&t[1]), ["Zed", "Alpha"]); // page 1 untouched
        assert_eq!(names(&t[2]), ["Utils", "apple", "Banana", "mango"]); // folders first, case-insensitive A→Z
        assert_eq!(names(&t[3])[1], "Yak"); // widget page untouched, still after
        let total = |ps: &[Value]| ps.iter().map(|p| p.as_array().unwrap().len()).sum::<usize>();
        assert_eq!(total(&t), total(&pages)); // nothing lost
    }
}
