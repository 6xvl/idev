//! Installed apps with sizes (installation_proxy Browse) and the Storage page built on them.

use crate::{card, fg, gb, hair, muted, soft, Job, ACCENT, BAD, GOOD, WARN};
use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense, Stroke};
use idevice::installation_proxy::InstallationProxyClient;
use plist::Value;

#[derive(Clone, Debug, Default)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// App bundle on disk (what "Offload App" frees).
    pub app_bytes: i64,
    /// Documents + caches (only "Delete App" frees this).
    pub data_bytes: i64,
    /// Code-signing identity. App Store apps: "Apple iPhone OS Application Signing". None = not reported.
    pub signer: Option<String>,
}

impl AppInfo {
    pub fn total(&self) -> i64 { self.app_bytes + self.data_bytes }
    /// Installed outside the App Store (developer / enterprise / sideload signature).
    pub fn sideloaded(&self) -> Option<bool> { self.signer.as_ref().map(|s| !s.starts_with("Apple iPhone OS Application Signing")) }
}

/// User-installed apps, biggest first.
pub async fn fetch() -> Result<Vec<AppInfo>, String> {
    let mut ip = crate::device::service::<InstallationProxyClient>().await?;
    let attrs = ["CFBundleIdentifier", "CFBundleDisplayName", "CFBundleName", "CFBundleShortVersionString", "StaticDiskUsage", "DynamicDiskUsage", "SignerIdentity"];
    let opts = Value::Dictionary(plist::Dictionary::from_iter([
        ("ApplicationType".to_string(), Value::String("User".into())),
        ("ReturnAttributes".to_string(), Value::Array(attrs.iter().map(|s| Value::String(s.to_string())).collect())),
    ]));
    let raw = ip.browse(Some(opts)).await.map_err(|e| format!("installation_proxy: {e}"))?;
    let mut apps: Vec<AppInfo> = raw.into_iter().filter_map(|v| {
        let d = v.into_dictionary()?;
        let s = |k: &str| d.get(k).and_then(|v| v.as_string()).map(str::to_string);
        Some(AppInfo {
            id: s("CFBundleIdentifier")?,
            name: s("CFBundleDisplayName").or_else(|| s("CFBundleName")).unwrap_or_default().trim_start_matches('\u{200e}').to_string(),
            version: s("CFBundleShortVersionString").unwrap_or_default(),
            app_bytes: crate::device::int_of(&d, "StaticDiskUsage").unwrap_or(0),
            data_bytes: crate::device::int_of(&d, "DynamicDiskUsage").unwrap_or(0),
            signer: s("SignerIdentity"),
        })
    }).collect();
    apps.sort_by_key(|a| -a.total());
    Ok(apps)
}

#[derive(Default)]
pub struct StoragePage {
    filter: String,
    sort_by_data: bool,
}

const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x2F, 0x6B, 0xFF), Color32::from_rgb(0x8B, 0x5C, 0xF6), Color32::from_rgb(0xEC, 0x48, 0x99), Color32::from_rgb(0xF5, 0x9E, 0x0B),
    Color32::from_rgb(0x10, 0xB9, 0x81), Color32::from_rgb(0x06, 0xB6, 0xD4), Color32::from_rgb(0xEF, 0x44, 0x44), Color32::from_rgb(0x84, 0xCC, 0x16),
];

impl StoragePage {
    pub fn ui(&mut self, ui: &mut egui::Ui, info: &crate::device::Info, apps: &Job<Vec<AppInfo>>) {
        let total = crate::device::int_of(&info.disk, "TotalDataCapacity").unwrap_or(0);
        let free = crate::device::int_of(&info.disk, "AmountDataAvailable").unwrap_or(0);
        let list = match apps.value() {
            None => { card(ui, |ui| ui.label(RichText::new("Measuring apps…").color(muted()))); return; }
            Some(Err(e)) => { card(ui, |ui| ui.colored_label(BAD, e)); return; }
            Some(Ok(l)) => l,
        };
        let apps_total: i64 = list.iter().map(AppInfo::total).sum();
        let other = (total - free - apps_total).max(0);

        // Summary + stacked bar of where the space goes.
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} used of {}", gb(total - free), gb(total))).size(16.0).strong().color(fg()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let pct = if total > 0 { free as f32 / total as f32 } else { 0.0 };
                    ui.label(RichText::new(format!("{} free ({:.0}%)", gb(free), pct * 100.0)).color(if pct < 0.05 { BAD } else if pct < 0.15 { WARN } else { GOOD }));
                });
            });
            ui.add_space(10.0);
            let top: Vec<&AppInfo> = list.iter().take(PALETTE.len()).collect();
            let rest: i64 = list.iter().skip(PALETTE.len()).map(AppInfo::total).sum();
            let mut segs: Vec<(String, i64, Color32)> = top.iter().enumerate().map(|(k, a)| (a.name.clone(), a.total(), PALETTE[k])).collect();
            segs.push((format!("{} other apps", list.len().saturating_sub(PALETTE.len())), rest, Color32::from_rgb(0x64, 0x74, 0x8B)));
            segs.push(("iOS, photos, messages & other".into(), other, Color32::from_rgb(0x47, 0x55, 0x69)));
            segs.push(("Free".into(), free, soft()));
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
            let mut x = rect.left();
            let p = ui.painter().clone();
            p.rect_filled(rect, CornerRadius::same(6), soft());
            for (name, bytes, col) in &segs {
                let w = rect.width() * (*bytes as f32 / total.max(1) as f32);
                if w < 0.5 { continue; }
                let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, rect.height()));
                p.rect_filled(seg, CornerRadius::ZERO, *col);
                ui.interact(seg, egui::Id::new(("seg", name)), Sense::hover()).on_hover_text(format!("{name}: {}", gb(*bytes)));
                x += w;
            }
            p.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, hair()), egui::StrokeKind::Inside);
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (name, bytes, col) in &segs {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(2), *col);
                    ui.label(RichText::new(format!("{name}  {}", gb(*bytes))).size(12.0).color(muted()));
                    ui.add_space(6.0);
                }
            });
        });
        ui.add_space(12.0);

        // What the user can actually do about it.
        let offload: i64 = list.iter().take(10).map(|a| a.app_bytes).sum();
        let caches: Vec<&AppInfo> = list.iter().filter(|a| a.data_bytes > 1_000_000_000).collect();
        card(ui, |ui| {
            ui.label(RichText::new("Ways to free space").strong().color(fg()));
            ui.add_space(6.0);
            ui.label(RichText::new(format!("•  Offloading the 10 biggest apps frees about {} and keeps their data (Settings › General › iPhone Storage).", gb(offload))).color(fg()));
            if !caches.is_empty() {
                let names: Vec<String> = caches.iter().take(5).map(|a| format!("{} ({})", a.name, gb(a.data_bytes))).collect();
                ui.label(RichText::new(format!("•  These apps hold over 1 GB of data each, usually caches and downloads: {}.", names.join(", "))).color(fg()));
            }
            ui.label(RichText::new(format!("•  \"iOS, photos, messages & other\" is {} — Photos and Messages attachments are the usual bulk.", gb(other))).color(muted()));
        });
        ui.add_space(12.0);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} apps", list.len())).strong().color(fg()));
                ui.add_space(12.0);
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search apps…").desired_width(220.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(&mut self.sort_by_data, "Sort by data");
                });
            });
            ui.add_space(8.0);
            let f = self.filter.to_lowercase();
            let mut rows: Vec<&AppInfo> = list.iter().filter(|a| f.is_empty() || a.name.to_lowercase().contains(&f) || a.id.to_lowercase().contains(&f)).collect();
            if self.sort_by_data { rows.sort_by_key(|a| -a.data_bytes); }
            let max = rows.iter().map(|a| a.total()).max().unwrap_or(1).max(1);
            egui::Grid::new("apps").num_columns(5).striped(true).spacing(vec2(14.0, 6.0)).show(ui, |ui| {
                for h in ["App", "Version", "App", "Data", ""] { ui.label(RichText::new(h).size(11.5).color(muted())); }
                ui.end_row();
                for a in rows {
                    ui.label(RichText::new(&a.name).color(fg())).on_hover_text(&a.id);
                    ui.label(RichText::new(&a.version).color(muted()).size(12.0));
                    ui.label(RichText::new(gb(a.app_bytes)).color(fg()));
                    ui.label(RichText::new(gb(a.data_bytes)).color(fg()));
                    let (r, _) = ui.allocate_exact_size(vec2(180.0, 8.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(r, CornerRadius::same(4), soft());
                    let wa = r.width() * a.app_bytes as f32 / max as f32;
                    let wd = r.width() * a.data_bytes as f32 / max as f32;
                    p.rect_filled(egui::Rect::from_min_size(r.min, vec2(wa, r.height())), CornerRadius::same(4), ACCENT);
                    p.rect_filled(egui::Rect::from_min_size(r.min + vec2(wa, 0.0), vec2(wd, r.height())), CornerRadius::same(4), Color32::from_rgb(0x8B, 0x5C, 0xF6));
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Blue = app files (freed by Offload), purple = documents & data (freed only by Delete).").size(11.5).color(muted()));
        });
    }
}
