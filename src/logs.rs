//! Live system log (syslog_relay) with search, errors-only and per-process filters.

use crate::{card, fg, muted, BAD, WARN};
use eframe::egui::{self, CornerRadius, FontId, RichText};
use idevice::syslog_relay::SyslogRelayClient;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const CAP: usize = 20_000;

#[derive(Default)]
struct State { lines: VecDeque<String>, running: bool, error: Option<String> }

#[derive(Default)]
pub struct Logs {
    st: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    filter: String,
    process: String,
    errors_only: bool,
    paused: Option<Vec<String>>,
    saved: Option<std::path::PathBuf>,
}

/// "Nov  9 01:50:37 host proc(Sub)[70] <Notice>: msg" -> (process, level).
pub fn parse(line: &str) -> (&str, &str) {
    let after_host = line.get(16..).and_then(|r| r.split_once(' ')).map_or("", |x| x.1);
    let proc_ = after_host.split(['[', '(']).next().unwrap_or("");
    let level = line.find(" <").and_then(|a| line[a + 2..].find('>').map(|b| &line[a + 2..a + 2 + b])).unwrap_or("");
    (proc_, level)
}

impl Logs {
    pub fn running(&self) -> bool { self.st.lock().unwrap().running }

    pub fn start(&mut self, rt: &tokio::runtime::Runtime, ctx: &egui::Context) {
        { let mut s = self.st.lock().unwrap(); if s.running { return; } s.running = true; s.error = None; }
        self.stop.store(false, Ordering::Relaxed);
        let (st, stop, ctx) = (self.st.clone(), self.stop.clone(), ctx.clone());
        rt.spawn(async move {
            let r: Result<(), String> = async {
                let mut c = crate::device::service::<SyslogRelayClient>().await?;
                let mut last = std::time::Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let Ok(l) = tokio::time::timeout(std::time::Duration::from_millis(500), c.next()).await else { continue };
                    let l = l.map_err(|e| format!("syslog: {e}"))?;
                    let mut s = st.lock().unwrap();
                    // One row per entry: the view uses fixed row heights, so fold multi-line messages.
                    s.lines.push_back(l.trim_end().replace('\n', "  ").replace('\t', " "));
                    while s.lines.len() > CAP { s.lines.pop_front(); }
                    drop(s);
                    if last.elapsed().as_millis() > 200 { ctx.request_repaint(); last = std::time::Instant::now(); }
                }
                Ok(())
            }.await;
            let mut s = st.lock().unwrap();
            s.running = false;
            if let Err(e) = r { s.error = Some(e); }
            ctx.request_repaint();
        });
    }

    pub fn stop(&self) { self.stop.store(true, Ordering::Relaxed); }

    pub fn ui(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Runtime) {
        let (running, error) = { let s = self.st.lock().unwrap(); (s.running, s.error.clone()) };
        if !running && error.is_none() && self.paused.is_none() && self.st.lock().unwrap().lines.is_empty() { self.start(rt, ui.ctx()); }

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search text…").desired_width(240.0));
                ui.add(egui::TextEdit::singleline(&mut self.process).hint_text("Process (e.g. SpringBoard)").desired_width(180.0));
                ui.checkbox(&mut self.errors_only, "Errors only");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save").clicked() {
                        let path = crate::docs_dir().join(format!("syslog-{}.txt", crate::stamp()));
                        let text: Vec<String> = self.st.lock().unwrap().lines.iter().cloned().collect();
                        if std::fs::write(&path, text.join("\n")).is_ok() { self.saved = Some(path); }
                    }
                    if ui.button("Clear").clicked() { self.st.lock().unwrap().lines.clear(); self.paused = None; }
                    let pause = if self.paused.is_some() { "Resume" } else { "Pause" };
                    if ui.button(pause).clicked() {
                        self.paused = match self.paused.take() { Some(_) => None, None => Some(self.st.lock().unwrap().lines.iter().cloned().collect()) };
                    }
                    if !running && ui.button("Reconnect").clicked() { self.start(rt, ui.ctx()); }
                });
            });
            if let Some(e) = &error { ui.colored_label(BAD, e); }
            if let Some(p) = &self.saved { if ui.link(format!("Saved to {}", p.display())).clicked() { crate::reveal(p); } }
        });
        ui.add_space(10.0);

        // Filter a snapshot (live buffer, or the frozen one while paused).
        let snapshot: Vec<String> = match &self.paused { Some(p) => p.clone(), None => self.st.lock().unwrap().lines.iter().cloned().collect() };
        let (f, pr) = (self.filter.to_lowercase(), self.process.to_lowercase());
        let shown: Vec<&String> = snapshot.iter().filter(|l| {
            let (p, lvl) = parse(l);
            (!self.errors_only || lvl == "Error" || lvl == "Fault")
                && (pr.is_empty() || p.to_lowercase().contains(&pr))
                && (f.is_empty() || l.to_lowercase().contains(&f))
        }).collect();

        ui.label(RichText::new(format!("{} of {} lines{}", shown.len(), snapshot.len(), if self.paused.is_some() { "  ·  paused" } else if running { "  ·  live" } else { "" })).size(12.0).color(muted()));
        let row_h = 16.0;
        egui::Frame::new().fill(crate::card_bg()).corner_radius(CornerRadius::same(10)).inner_margin(10).stroke(egui::Stroke::new(1.0, crate::hair())).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::both().auto_shrink([false, false]).max_height(ui.available_height().max(300.0)).stick_to_bottom(self.paused.is_none())
                .show_rows(ui, row_h, shown.len(), |ui, range| {
                    for l in &shown[range] {
                        let (_, lvl) = parse(l);
                        let col = match lvl { "Error" | "Fault" => BAD, "Warning" => WARN, "Debug" | "Info" => muted(), _ => fg() };
                        ui.add(egui::Label::new(RichText::new(l.as_str()).font(FontId::monospace(11.5)).color(col)).extend());
                    }
                });
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_line() {
        let l = "Nov  9 01:50:37 iPhone backboardd(ColourSensorFilterPlugin)[70] <Notice>: [ALS] ts=1";
        assert_eq!(super::parse(l), ("backboardd", "Notice"));
        assert_eq!(super::parse("Nov 10 12:00:00 phone SpringBoard[55] <Error>: boom"), ("SpringBoard", "Error"));
        assert_eq!(super::parse("short"), ("", ""));
    }
}
