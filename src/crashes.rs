//! Crash insights: pull every .ips report, classify it, and say what is going on in plain words.

use crate::{card, fg, muted, soft, Job, ACCENT, BAD, GOOD, WARN};
use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense};
use idevice::crashreportcopymobile::CrashReportCopyMobileClient;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind { Crash, Memory, Cpu, Disk, Hang, Other }

impl Kind {
    pub fn label(self) -> &'static str {
        match self { Kind::Crash => "Crash", Kind::Memory => "Killed for memory", Kind::Cpu => "CPU overuse", Kind::Disk => "Heavy disk writes", Kind::Hang => "Freeze / hang", Kind::Other => "System log" }
    }
    fn color(self) -> Color32 {
        match self { Kind::Crash => BAD, Kind::Memory => Color32::from_rgb(0x8B, 0x5C, 0xF6), Kind::Cpu => WARN, Kind::Disk => Color32::from_rgb(0x06, 0xB6, 0xD4), Kind::Hang => Color32::from_rgb(0xEC, 0x48, 0x99), Kind::Other => Color32::from_rgb(0x64, 0x74, 0x8B) }
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub file: String,
    pub kind: Kind,
    /// App the report is about (for memory kills: the largest process iOS killed).
    pub app: String,
    pub date: String,
}

/// Classify from the file name, which iOS builds as "<Process>.<type>-YYYY-MM-DD-HHMMSS.ips".
pub fn kind_of(file: &str) -> Kind {
    if file.starts_with("JetsamEvent") { Kind::Memory }
    else if file.contains("cpu_resource") || file.contains("wakeups_resource") { Kind::Cpu }
    else if file.contains("diskwrites_resource") { Kind::Disk }
    else if file.starts_with("stacks") || file.contains(".hang") || file.contains("Hang") { Kind::Hang }
    // iOS's own logs that are not failures of any app.
    else if file.contains("Analytics") || file.contains("SiriSearchFeedback") || file.starts_with("LowBatteryLog") || !file.ends_with(".ips") { Kind::Other }
    else { Kind::Crash }
}

/// Value of `"key":"value"` or `"key" : "value"` in a JSON-ish header/body (avoids a JSON dependency for one field).
pub fn json_str<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let at = text.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    rest.split('"').next()
}

pub fn parse(file: &str, body: &str) -> Report {
    let kind = kind_of(file);
    let head = body.lines().next().unwrap_or("");
    let app = match kind {
        Kind::Memory => json_str(body, "largestProcess").map(str::to_string),
        _ => json_str(head, "app_name").or_else(|| json_str(head, "name")).map(str::to_string),
    }.unwrap_or_else(|| file.split(['.', '-']).next().unwrap_or(file).to_string());
    let date = json_str(head, "timestamp").map(|t| t.chars().take(16).collect()).or_else(|| crate::date_in(file).map(str::to_string)).unwrap_or_default();
    Report { file: file.to_string(), kind, app, date }
}

/// Every report in the top folder, parsed. Memory reports are 200 KB each; only the first 64 KB is read for the header and largest process.
pub async fn fetch() -> Result<Vec<Report>, String> {
    let mut c = crate::device::service::<CrashReportCopyMobileClient>().await?;
    let names = c.ls(None).await.map_err(|e| format!("crash list: {e}"))?;
    let mut out = Vec::new();
    for n in names.into_iter().filter(|n| n.ends_with(".ips")) {
        let body = c.pull(n.clone()).await.map(|b| String::from_utf8_lossy(&b[..b.len().min(65_536)]).into_owned()).unwrap_or_default();
        out.push(parse(&n, &body));
    }
    out.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(out)
}

/// Copy all reports to Documents\idev\reports-<stamp> and return the folder.
pub async fn export() -> Result<std::path::PathBuf, String> {
    let mut c = crate::device::service::<CrashReportCopyMobileClient>().await?;
    let dir = crate::docs_dir().join(format!("reports-{}", crate::stamp()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for n in c.ls(None).await.map_err(|e| e.to_string())?.into_iter().filter(|n| n.ends_with(".ips")) {
        if let Ok(b) = c.pull(n.clone()).await { let _ = std::fs::write(dir.join(&n), b); }
    }
    Ok(dir)
}

#[derive(Default)]
pub struct CrashPage {
    exported: Job<std::path::PathBuf>,
    show: Option<Kind>,
}

impl CrashPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Runtime, list: &Job<Vec<Report>>) {
        self.exported.poll();
        let reports = match list.value() {
            None => { card(ui, |ui| ui.label(RichText::new("Reading crash reports from the phone…").color(muted()))); return; }
            Some(Err(e)) => { card(ui, |ui| ui.colored_label(BAD, e)); return; }
            Some(Ok(r)) => r,
        };

        // Per app, per kind.
        let mut by_app: BTreeMap<&str, BTreeMap<Kind, usize>> = BTreeMap::new();
        for r in reports { *by_app.entry(&r.app).or_default().entry(r.kind).or_default() += 1; }
        let count = |k: Kind| reports.iter().filter(|r| r.kind == k).count();
        let top = |k: Kind| by_app.iter().filter_map(|(a, m)| m.get(&k).map(|n| (*a, *n))).max_by_key(|x| x.1);

        card(ui, |ui| {
            ui.label(RichText::new("What the reports say").strong().color(fg()));
            ui.add_space(6.0);
            let mut said = false;
            if let Some((a, n)) = top(Kind::Crash) { ui.label(RichText::new(format!("•  {a} crashed the most: {n} time{}.", if n == 1 { "" } else { "s" })).color(fg())); said = true; }
            let mem = count(Kind::Memory);
            if mem > 0 {
                let who = top(Kind::Memory).map(|(a, _)| format!(" The biggest app at the time was usually {a}.")).unwrap_or_default();
                ui.label(RichText::new(format!("•  iOS ran out of memory {mem} time{} and closed apps in the background.{who}", if mem == 1 { "" } else { "s" })).color(fg()));
                said = true;
            }
            if let Some((a, n)) = top(Kind::Cpu) { ui.label(RichText::new(format!("•  {a} used too much CPU in the background {n} time{} (battery drain).", if n == 1 { "" } else { "s" })).color(fg())); said = true; }
            if let Some((a, n)) = top(Kind::Disk) { ui.label(RichText::new(format!("•  {a} wrote unusually much to storage {n} time{} (wears flash, slows the phone).", if n == 1 { "" } else { "s" })).color(fg())); said = true; }
            if count(Kind::Hang) > 0 { ui.label(RichText::new(format!("•  {} freeze report{} (the screen stopped responding).", count(Kind::Hang), if count(Kind::Hang) == 1 { "" } else { "s" })).color(fg())); said = true; }
            if !said { ui.label(RichText::new("No problems on record. iOS keeps reports for a few weeks.").color(GOOD)); }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(if self.exported.running() { "Exporting…" } else { "Export all reports" }).clicked() {
                    self.exported.clear();
                    self.exported.start(rt, ui.ctx(), export());
                }
                match self.exported.value() {
                    Some(Ok(d)) => { if ui.link(format!("Open {}", d.display())).clicked() { crate::reveal(d); } }
                    Some(Err(e)) => { ui.colored_label(BAD, e); }
                    None => {}
                }
            });
        });
        ui.add_space(12.0);

        // Last 14 days, stacked by kind.
        card(ui, |ui| {
            ui.label(RichText::new("Last 14 days").strong().color(fg()));
            ui.add_space(6.0);
            let today = crate::device::today();
            let days: Vec<String> = (0..14).rev().map(|k| crate::device::civil(today - k)).collect();
            let mut per: Vec<BTreeMap<Kind, usize>> = vec![BTreeMap::new(); 14];
            for r in reports { if let Some(i) = days.iter().position(|d| r.date.starts_with(d.as_str())) { *per[i].entry(r.kind).or_default() += 1; } }
            let max = per.iter().map(|m| m.values().sum::<usize>()).max().unwrap_or(0).max(1);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 120.0), Sense::hover());
            let p = ui.painter();
            let bw = rect.width() / 14.0;
            for (i, m) in per.iter().enumerate() {
                let mut y = rect.bottom() - 16.0;
                for (k, n) in m {
                    let h = (rect.height() - 22.0) * *n as f32 / max as f32;
                    p.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left() + i as f32 * bw + 3.0, y - h), egui::pos2(rect.left() + (i + 1) as f32 * bw - 3.0, y)), CornerRadius::same(2), k.color());
                    y -= h;
                }
                if i % 2 == 1 { p.text(egui::pos2(rect.left() + (i as f32 + 0.5) * bw, rect.bottom()), egui::Align2::CENTER_BOTTOM, &days[i][5..], egui::FontId::proportional(10.0), muted()); }
            }
            ui.horizontal_wrapped(|ui| {
                for k in [Kind::Crash, Kind::Memory, Kind::Cpu, Kind::Disk, Kind::Hang, Kind::Other] {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(2), k.color());
                    ui.label(RichText::new(format!("{} ({})", k.label(), count(k))).size(12.0).color(muted()));
                }
            });
        });
        ui.add_space(12.0);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} reports", reports.len())).strong().color(fg()));
                ui.add_space(10.0);
                for k in [None, Some(Kind::Crash), Some(Kind::Memory), Some(Kind::Cpu), Some(Kind::Disk), Some(Kind::Hang)] {
                    let sel = self.show == k;
                    let b = egui::Button::new(RichText::new(k.map_or("All", Kind::label)).size(12.0).color(if sel { ACCENT } else { fg() })).fill(if sel { soft() } else { Color32::TRANSPARENT }).corner_radius(CornerRadius::same(10));
                    if ui.add(b).clicked() { self.show = k; }
                }
            });
            ui.add_space(6.0);
            egui::Grid::new("reports").num_columns(3).striped(true).spacing(vec2(16.0, 5.0)).show(ui, |ui| {
                for r in reports.iter().filter(|r| self.show.is_none_or(|k| r.kind == k)) {
                    ui.label(RichText::new(&r.date).size(12.0).color(muted()));
                    ui.label(RichText::new(r.kind.label()).size(12.0).color(r.kind.color()));
                    ui.label(RichText::new(&r.app).color(fg())).on_hover_text(&r.file);
                    ui.end_row();
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_and_parse() {
        assert_eq!(kind_of("JetsamEvent-2026-10-30-160350.ips"), Kind::Memory);
        assert_eq!(kind_of("Discord.diskwrites_resource-2026-10-31-064826.ips"), Kind::Disk);
        assert_eq!(kind_of("coreduetd.cpu_resource-2026-10-23-082034.ips"), Kind::Cpu);
        assert_eq!(kind_of("TikTok-2026-11-01-101010.ips"), Kind::Crash);
        assert_eq!(kind_of("LowBatteryLog-2026-11-08-155800.ips"), Kind::Other);
        let r = parse("Discord.diskwrites_resource-2026-10-31-064826.ips", "{\"roots_installed\":0,\"app_name\":\"Discord\",\"timestamp\":\"2026-10-31 06:48:26.00 +0100\"}\nrest");
        assert_eq!((r.app.as_str(), r.date.as_str()), ("Discord", "2026-10-31 06:48"));
        let j = parse("JetsamEvent-2026-10-30-160350.ips", "{\"bug_type\":\"298\",\"timestamp\":\"2026-10-30 16:03:50.00 +0100\"}\n{\n  \"largestProcess\" : \"TikTok\",\n}");
        assert_eq!((j.app.as_str(), j.kind), ("TikTok", Kind::Memory));
        let unknown = parse("Foo-2026-01-02-030405.ips", "");
        assert_eq!((unknown.app.as_str(), unknown.date.as_str()), ("Foo", "2026-01-02"));
    }
}
