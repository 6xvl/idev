//! Network capture: the phone's pcapd stream -> a Wireshark-ready .pcap, plus live traffic per process.

use crate::{card, fg, muted, soft, ACCENT, BAD, GOOD};
use eframe::egui::{self, vec2, CornerRadius, RichText, Sense};
use idevice::pcapd::PcapdClient;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Classic pcap file, Ethernet link type (pcapd packets are normalised to Ethernet frames first).
pub struct Pcap<W: Write> { w: W }

impl<W: Write> Pcap<W> {
    pub fn new(mut w: W) -> std::io::Result<Self> {
        // magic, version 2.4, tz 0, sigfigs 0, snaplen 262144, LINKTYPE_ETHERNET = 1. Little-endian.
        w.write_all(&0xa1b2_c3d4u32.to_le_bytes())?;
        w.write_all(&2u16.to_le_bytes())?;
        w.write_all(&4u16.to_le_bytes())?;
        w.write_all(&0i32.to_le_bytes())?;
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(&262_144u32.to_le_bytes())?;
        w.write_all(&1u32.to_le_bytes())?;
        Ok(Self { w })
    }
    pub fn packet(&mut self, secs: u32, usecs: u32, data: &[u8]) -> std::io::Result<()> {
        for v in [secs, usecs, data.len() as u32, data.len() as u32] { self.w.write_all(&v.to_le_bytes())?; }
        self.w.write_all(data)
    }
    pub fn finish(mut self) -> std::io::Result<W> { self.w.flush()?; Ok(self.w) }
}

#[derive(Default)]
struct State {
    by_proc: HashMap<String, (u64, u64)>, // packets, bytes
    packets: u64,
    bytes: u64,
    file: Option<std::path::PathBuf>,
    error: Option<String>,
    running: bool,
}

#[derive(Default)]
pub struct Net {
    st: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

async fn capture(st: Arc<Mutex<State>>, stop: Arc<AtomicBool>, ctx: egui::Context) -> Result<(), String> {
    let mut client = crate::device::service::<PcapdClient>().await?;
    let path = crate::docs_dir().join(format!("capture-{}.pcap", crate::stamp()));
    let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut pcap = Pcap::new(std::io::BufWriter::new(file)).map_err(|e| e.to_string())?;
    st.lock().unwrap().file = Some(path);
    let mut last_paint = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        // Time out so Stop works on a quiet phone.
        let Ok(next) = tokio::time::timeout(std::time::Duration::from_millis(500), client.next_packet()).await else { continue };
        let mut pk = next.map_err(|e| format!("pcapd: {e}"))?;
        pk.normalize_data();
        pcap.packet(pk.seconds, pk.microseconds, &pk.data).map_err(|e| e.to_string())?;
        let who = if !pk.comm.is_empty() { pk.comm.clone() } else if !pk.ecomm.is_empty() { pk.ecomm.clone() } else { "(kernel / unknown)".into() };
        {
            let mut s = st.lock().unwrap();
            let e = s.by_proc.entry(who).or_default();
            e.0 += 1;
            e.1 += pk.data.len() as u64;
            s.packets += 1;
            s.bytes += pk.data.len() as u64;
        }
        if last_paint.elapsed().as_millis() > 250 { ctx.request_repaint(); last_paint = std::time::Instant::now(); }
    }
    pcap.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn human(b: u64) -> String {
    match b { b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64), b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64), b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0), b => format!("{b} B") }
}

impl Net {
    fn start(&mut self, rt: &tokio::runtime::Runtime, ctx: &egui::Context) {
        *self.st.lock().unwrap() = State { running: true, ..Default::default() };
        self.stop.store(false, Ordering::Relaxed);
        let (st, stop, ctx) = (self.st.clone(), self.stop.clone(), ctx.clone());
        rt.spawn(async move {
            let r = capture(st.clone(), stop, ctx.clone()).await;
            let mut s = st.lock().unwrap();
            s.running = false;
            if let Err(e) = r { s.error = Some(e); }
            ctx.request_repaint();
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, rt: &tokio::runtime::Runtime) {
        let (running, packets, bytes, file, error, mut procs) = {
            let s = self.st.lock().unwrap();
            (s.running, s.packets, s.bytes, s.file.clone(), s.error.clone(), s.by_proc.iter().map(|(k, v)| (k.clone(), *v)).collect::<Vec<_>>())
        };
        procs.sort_by_key(|p| std::cmp::Reverse(p.1 .1));

        card(ui, |ui| {
            ui.horizontal(|ui| {
                let label = if running { "Stop capture" } else { "Start capture" };
                if ui.add(egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(if running { BAD } else { ACCENT }).corner_radius(CornerRadius::same(8)).min_size(vec2(120.0, 30.0))).clicked() {
                    if running { self.stop.store(true, Ordering::Relaxed); } else { self.start(rt, ui.ctx()); }
                }
                ui.add_space(10.0);
                if running {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, BAD);
                    ui.label(RichText::new(format!("Recording  ·  {packets} packets  ·  {}", human(bytes))).color(fg()));
                } else if packets > 0 {
                    ui.label(RichText::new(format!("Saved {packets} packets ({})", human(bytes))).color(GOOD));
                }
                if let Some(f) = &file {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.link("Show file").clicked() { crate::reveal(f); }
                    });
                }
            });
            if let Some(e) = &error { ui.colored_label(BAD, e); }
            ui.add_space(6.0);
            ui.label(RichText::new("Records every packet the phone sends or receives on all interfaces (Wi-Fi, cellular, VPN) into a .pcap you can open in Wireshark. Each packet is tagged with the app that sent it.").size(12.0).color(muted()));
            if let Some(f) = &file { ui.label(RichText::new(f.display().to_string()).size(11.0).color(muted())); }
        });
        ui.add_space(12.0);

        card(ui, |ui| {
            ui.label(RichText::new("Traffic by app").strong().color(fg()));
            ui.add_space(6.0);
            if procs.is_empty() {
                ui.label(RichText::new(if running { "Waiting for traffic…" } else { "Start a capture to see which apps use the network." }).color(muted()));
                return;
            }
            let max = procs.first().map_or(1, |p| p.1 .1).max(1);
            egui::Grid::new("procs").num_columns(4).striped(true).spacing(vec2(16.0, 5.0)).show(ui, |ui| {
                for h in ["App / process", "Packets", "Data", ""] { ui.label(RichText::new(h).size(11.5).color(muted())); }
                ui.end_row();
                for (name, (n, b)) in procs.iter().take(40) {
                    ui.label(RichText::new(name).color(fg()));
                    ui.label(RichText::new(n.to_string()).color(muted()));
                    ui.label(RichText::new(human(*b)).color(fg()));
                    let (r, _) = ui.allocate_exact_size(vec2(220.0, 8.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(4), soft());
                    ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(r.width() * *b as f32 / max as f32, r.height())), CornerRadius::same(4), ACCENT);
                    ui.end_row();
                }
            });
        });
    }

    pub fn capturing(&self) -> bool { self.st.lock().unwrap().running }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pcap_layout() {
        let mut p = super::Pcap::new(Vec::new()).unwrap();
        p.packet(10, 20, &[1, 2, 3]).unwrap();
        let b = p.finish().unwrap();
        assert_eq!(b.len(), 24 + 16 + 3);
        assert_eq!(&b[..4], &[0xd4, 0xc3, 0xb2, 0xa1]); // little-endian magic, what Wireshark expects
        assert_eq!(u32::from_le_bytes(b[20..24].try_into().unwrap()), 1); // Ethernet
        assert_eq!(u32::from_le_bytes(b[32..36].try_into().unwrap()), 3); // incl_len
        assert_eq!(&b[40..], &[1, 2, 3]);
    }
}
