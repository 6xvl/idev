//! Battery page: live power graph from the 5 s polls, plus a per-phone health history on disk with a forecast.

use crate::device::{int_of, Info};
use crate::{card, fg, hair, muted, row, soft, ACCENT, BAD, GOOD, WARN};
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, FontId, RichText, Sense, Shape, Stroke};
use std::collections::VecDeque;

const KEEP: usize = 720; // 1 hour at 5 s

#[derive(Clone, Copy)]
struct Sample { t: f64, watts: f32, temp: f32, pct: f32 }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistRow { pub day: i64, pub health: f32, pub cycles: i64 }

#[derive(Default)]
pub struct Power {
    samples: VecDeque<Sample>,
    last_update: i64,
    start: Option<std::time::Instant>,
    udid: String,
    history: Vec<HistRow>,
}

fn history_path(udid: &str) -> std::path::PathBuf { crate::data_dir().join(format!("battery-{udid}.csv")) }

pub fn parse_history(text: &str) -> Vec<HistRow> {
    text.lines().filter_map(|l| {
        let mut f = l.split(',');
        Some(HistRow { day: crate::device::days_from_civil(f.next()?)?, health: f.next()?.parse().ok()?, cycles: f.next()?.parse().ok()? })
    }).collect()
}

/// Least-squares line through (day, health); returns the day health reaches `target`. Needs 2+ points over 14+ days and a falling trend.
pub fn forecast(h: &[HistRow], target: f32) -> Option<i64> {
    if h.len() < 2 || h.last()?.day - h.first()?.day < 14 { return None; }
    let n = h.len() as f64;
    let (mx, my) = (h.iter().map(|r| r.day as f64).sum::<f64>() / n, h.iter().map(|r| r.health as f64).sum::<f64>() / n);
    let sxy: f64 = h.iter().map(|r| (r.day as f64 - mx) * (r.health as f64 - my)).sum();
    let sxx: f64 = h.iter().map(|r| (r.day as f64 - mx).powi(2)).sum();
    let slope = sxy / sxx;
    if slope >= 0.0 { return None; }
    Some((mx + (target as f64 - my) / slope).round() as i64)
}

impl Power {
    /// Called with every fresh poll. Dedupes on the gauge's own UpdateTime; writes one history row per phone per day.
    pub fn on_info(&mut self, i: &Info) {
        let p = &i.power;
        let upd = int_of(p, "UpdateTime").unwrap_or(0);
        if upd != 0 && upd == self.last_update { return; }
        self.last_update = upd;
        let start = *self.start.get_or_insert_with(std::time::Instant::now);
        let (mv, ma) = (int_of(p, "Voltage").unwrap_or(0) as f32, int_of(p, "InstantAmperage").unwrap_or(0) as f32);
        self.samples.push_back(Sample {
            t: start.elapsed().as_secs_f64(),
            watts: mv * ma / 1e6,
            temp: int_of(p, "Temperature").unwrap_or(0) as f32 / 100.0,
            pct: int_of(&i.battery, "BatteryCurrentCapacity").unwrap_or(0) as f32,
        });
        while self.samples.len() > KEEP { self.samples.pop_front(); }

        if self.udid != i.udid {
            self.udid = i.udid.clone();
            self.history = std::fs::read_to_string(history_path(&self.udid)).map(|t| parse_history(&t)).unwrap_or_default();
        }
        let (Some(n), Some(d), Some(c)) = (int_of(p, "NominalChargeCapacity"), int_of(p, "DesignCapacity"), int_of(p, "CycleCount")) else { return };
        let today = crate::device::today();
        if d > 0 && self.history.last().map_or(true, |r| r.day != today) {
            let r = HistRow { day: today, health: n as f32 * 100.0 / d as f32, cycles: c };
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(history_path(&self.udid)) {
                let _ = writeln!(f, "{},{:.2},{}", crate::device::civil(r.day), r.health, r.cycles);
            }
            self.history.push(r);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, i: &Info) {
        let p = &i.power;
        let charging = i.battery.get("BatteryIsCharging").and_then(|v| v.as_boolean()).unwrap_or(false);
        let plugged = i.battery.get("ExternalConnected").and_then(|v| v.as_boolean()).unwrap_or(false);
        let last = self.samples.back().copied();
        let watts = last.map_or(0.0, |s| s.watts);

        ui.columns(2, |c| {
            card(&mut c[0], |ui| {
                ui.label(RichText::new("Right now").strong().color(fg()));
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{:+.1} W", watts)).size(30.0).strong().color(if watts > 0.0 { GOOD } else { fg() }));
                ui.label(RichText::new(if charging { "into the battery" } else if plugged { "plugged in, not charging" } else { "drawn from the battery" }).color(muted()));
                ui.add_space(8.0);
                // Charger verdict from the measured power, with the adapter's own report when the gauge has one.
                let adapter = p.get("AdapterDetails").and_then(|v| v.as_dictionary());
                let rated = adapter.and_then(|a| int_of(a, "Watts"));
                // iPhones fast-charge only up to ~50%, then taper on purpose, so low watts above 50% are normal.
                let pct = int_of(&i.battery, "BatteryCurrentCapacity").unwrap_or(0);
                let verdict = if !plugged || !charging { None }
                    else if pct >= 50 { Some(("Charging slows above 50% to protect the battery; low watts here are normal.", muted())) }
                    else if watts < 4.0 { Some(("Slow charging: under 4 W below 50%. Likely a 5 W brick, a weak cable or a USB port.", WARN)) }
                    else if watts >= 15.0 { Some(("Fast charging.", GOOD)) } else { Some(("Normal charging speed.", GOOD)) };
                if let Some((t, col)) = verdict { ui.label(RichText::new(t).color(col)); }
                if let Some(w) = rated { row(ui, "Adapter rating", format!("{w} W")); }
                if let Some(d) = adapter.and_then(|a| a.get("Description")).and_then(|v| v.as_string()) { row(ui, "Adapter", d.to_string()); }
                // Time to full: remaining mAh / current mA.
                let (nom, cur, ma) = (int_of(p, "NominalChargeCapacity"), int_of(p, "AppleRawCurrentCapacity"), int_of(p, "InstantAmperage"));
                if let (true, Some(n), Some(c), Some(a)) = (charging, nom, cur, ma) {
                    if a > 50 { let h = (n - c).max(0) as f32 / a as f32; row(ui, "Time to full (at this rate)", format!("{}h {:02}m", h as i32, (h.fract() * 60.0) as i32)); }
                }
                if !charging {
                    if let Some(m) = int_of(p, "AvgTimeToEmpty").filter(|&m| m > 0 && m < 65_535) { row(ui, "Time to empty (iOS estimate)", format!("{}h {:02}m", m / 60, m % 60)); }
                }
                row(ui, "Voltage", int_of(p, "Voltage").map_or("?".into(), |v| format!("{:.2} V", v as f32 / 1000.0)));
                row(ui, "Current", int_of(p, "InstantAmperage").map_or("?".into(), |v| format!("{v} mA")));
                row(ui, "Temperature", last.map_or("?".into(), |s| format!("{:.1} °C", s.temp)));
            });
            card(&mut c[1], |ui| {
                ui.label(RichText::new("Last hour").strong().color(fg()));
                ui.add_space(6.0);
                let pts = |f: fn(&Sample) -> f32| self.samples.iter().map(|s| (s.t, f(s))).collect::<Vec<_>>();
                chart(ui, "Power (W)", &pts(|s| s.watts), ACCENT, 90.0);
                chart(ui, "Battery (%)", &pts(|s| s.pct), GOOD, 70.0);
                chart(ui, "Temperature (°C)", &pts(|s| s.temp), WARN, 70.0);
                if self.samples.len() < 3 { ui.label(RichText::new("Graphs fill in every 5 seconds while idev is open.").size(11.5).color(muted())); }
            });
        });
        ui.add_space(12.0);

        let mah = |k: &str| int_of(p, k).map_or("?".into(), |v| format!("{v} mAh"));
        ui.columns(2, |c| {
            card(&mut c[0], |ui| {
                ui.label(RichText::new("Health history").strong().color(fg()));
                ui.add_space(6.0);
                let pts: Vec<(f64, f32)> = self.history.iter().map(|r| (r.day as f64, r.health)).collect();
                if pts.len() >= 2 { chart(ui, "Health (%)", &pts, GOOD, 110.0); }
                let first = self.history.first();
                match first {
                    Some(f) => { let n = self.history.len(); ui.label(RichText::new(format!("{n} day{} recorded since {}.", if n == 1 { "" } else { "s" }, crate::device::civil(f.day))).color(muted())); }
                    None => { ui.label(RichText::new("Recording starts with this connection.").color(muted())); }
                }
                match (forecast(&self.history, 75.0), forecast(&self.history, 70.0)) {
                    (Some(a), b) => {
                        row(ui, "Reaches 75% around", crate::device::civil(a));
                        if let Some(b) = b { row(ui, "Reaches 70% around", crate::device::civil(b)); }
                    }
                    _ => { ui.label(RichText::new("A forecast appears once two weeks of readings exist (one per day the phone is connected).").size(12.0).color(muted())); }
                }
                let path = history_path(&i.udid);
                if ui.add(egui::Label::new(RichText::new(format!("Saved in {}", path.display())).size(11.0).color(muted())).truncate().sense(Sense::click()))
                    .on_hover_text("Show in Explorer").clicked() { crate::reveal(&path); }
            });
            card(&mut c[1], |ui| {
                ui.label(RichText::new("Battery details").strong().color(fg()));
                ui.add_space(6.0);
                row(ui, "Design capacity", mah("DesignCapacity"));
                row(ui, "Full charge capacity", mah("NominalChargeCapacity"));
                row(ui, "Raw max capacity", mah("AppleRawMaxCapacity"));
                row(ui, "Current charge", mah("AppleRawCurrentCapacity"));
                row(ui, "Cycle count", int_of(p, "CycleCount").map_or("?".into(), |v| v.to_string()));
                row(ui, "Battery serial", crate::device::str_of(p.get("Serial")));
                let h = crate::health(i);
                if let Some(h) = h {
                    ui.add_space(4.0);
                    ui.label(RichText::new(if h >= 0.8 { "Apple treats 80% and up as healthy." } else { "Below 80%: Apple considers this battery due for service." })
                        .size(12.0).color(if h >= 0.8 { muted() } else { BAD }));
                }
            });
        });
    }
}

/// Small line chart: title, min/max labels, one series. x values only need to be increasing.
fn chart(ui: &mut egui::Ui, title: &str, pts: &[(f64, f32)], col: Color32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(6), soft());
    p.text(rect.left_top() + vec2(8.0, 6.0), egui::Align2::LEFT_TOP, title, FontId::proportional(11.0), muted());
    if pts.len() < 2 { ui.add_space(6.0); return; }
    let (x0, x1) = (pts[0].0, pts[pts.len() - 1].0.max(pts[0].0 + 1e-6));
    let (mut lo, mut hi) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), &(_, y)| (a.min(y), b.max(y)));
    if hi - lo < 1e-3 { lo -= 1.0; hi += 1.0; }
    let plot = rect.shrink2(vec2(8.0, 0.0)).with_min_y(rect.top() + 22.0).with_max_y(rect.bottom() - 6.0);
    let line: Vec<_> = pts.iter().map(|&(x, y)| pos2(
        plot.left() + plot.width() * ((x - x0) / (x1 - x0)) as f32,
        plot.bottom() - plot.height() * (y - lo) / (hi - lo),
    )).collect();
    p.add(Shape::line(line, Stroke::new(1.8, col)));
    p.text(rect.right_top() + vec2(-8.0, 6.0), egui::Align2::RIGHT_TOP, format!("{hi:.1}"), FontId::proportional(10.5), muted());
    p.text(rect.right_bottom() + vec2(-8.0, -4.0), egui::Align2::RIGHT_BOTTOM, format!("{lo:.1}"), FontId::proportional(10.5), muted());
    p.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, hair()), egui::StrokeKind::Inside);
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_roundtrip_and_forecast() {
        let h = parse_history("2026-01-01,90.00,500\n2026-03-02,88.00,560\nbad line\n2026-05-01,86.00,620\n");
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].cycles, 500);
        // About -2 points per 60 days: 86 -> 75 is 11 points, roughly 330 days after 2026-05-01.
        let d = forecast(&h, 75.0).unwrap();
        assert!((d - (h[2].day + 330)).abs() <= 8, "{}", crate::device::civil(d));
        assert_eq!(forecast(&h[..1], 75.0), None);
        let flat = parse_history("2026-01-01,90,1\n2026-02-01,91,2\n");
        assert_eq!(forecast(&flat, 75.0), None);
    }
}
