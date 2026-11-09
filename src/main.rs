//! idev: a small 3uTools-style iPhone dashboard. `idev --dump` prints the raw data instead.
#![windows_subsystem = "windows"]
mod apps;
mod crashes;
mod device;
mod home;
mod logs;
mod netcap;
mod power;
mod security;

use device::{int_of, str_of, Info};
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Margin, Rect, RichText, Sense, Shape, Stroke};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(0x2F, 0x6B, 0xFF);
const GOOD: Color32 = Color32::from_rgb(0x22, 0xB5, 0x73);
const WARN: Color32 = Color32::from_rgb(0xF5, 0xA6, 0x23);
const BAD: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);
const REFRESH: Duration = Duration::from_secs(5);

// Theme: neutral colours switch at runtime (dark by default); accent and status colours are shared.
static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
fn dark() -> bool { DARK.load(std::sync::atomic::Ordering::Relaxed) }
fn pick(light: Color32, dark_c: Color32) -> Color32 { if dark() { dark_c } else { light } }
fn bg() -> Color32 { pick(Color32::from_rgb(0xF2, 0xF5, 0xFA), Color32::from_rgb(0x0E, 0x10, 0x14)) }
fn card_bg() -> Color32 { pick(Color32::WHITE, Color32::from_rgb(0x17, 0x1A, 0x20)) }
fn side() -> Color32 { pick(Color32::from_rgb(0xEA, 0xEE, 0xF4), Color32::from_rgb(0x12, 0x14, 0x19)) }
fn hair() -> Color32 { pick(Color32::from_rgb(0xDE, 0xE3, 0xEB), Color32::from_rgb(0x25, 0x29, 0x31)) }
fn soft() -> Color32 { pick(Color32::from_rgb(0xEC, 0xEF, 0xF4), Color32::from_rgb(0x22, 0x26, 0x2E)) }
fn fg() -> Color32 { pick(Color32::from_rgb(0x1E, 0x24, 0x2E), Color32::from_rgb(0xE6, 0xE9, 0xEF)) }
fn muted() -> Color32 { pick(Color32::from_rgb(0x7A, 0x84, 0x94), Color32::from_rgb(0x87, 0x90, 0xA0)) }
fn good_bg() -> Color32 { pick(Color32::from_rgb(0xE6, 0xF6, 0xEE), Color32::from_rgb(0x12, 0x2A, 0x1F)) }
fn warn_bg() -> Color32 { pick(Color32::from_rgb(0xFD, 0xF3, 0xE1), Color32::from_rgb(0x30, 0x25, 0x10)) }

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = if dark() { egui::Visuals::dark() } else { egui::Visuals::light() };
    style.visuals.panel_fill = bg();
    style.visuals.extreme_bg_color = soft(); // text edits
    style.visuals.widgets.inactive.weak_bg_fill = soft();
    style.spacing.item_spacing = vec2(8.0, 6.0);
    ctx.set_style(style);
}

#[derive(Default)]
struct Shared {
    info: Option<Info>, error: Option<String>, busy: bool, status: Option<String>,
    /// Decoded home screen waiting to be uploaded as a texture by the UI thread.
    home: Option<egui::ColorImage>,
    extras: Option<device::Extras>,
    home_busy: bool,
}

/// PNG -> RGBA, shrunk so the GPU never minifies 10x (that aliases). `max` is the longest side in px.
fn decode(png: Vec<u8>, max: u32) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(&png).ok()?.to_rgba8();
    let img = if img.width().max(img.height()) > max { image::imageops::thumbnail(&img, max * img.width() / img.width().max(img.height()), max * img.height() / img.width().max(img.height())) } else { img };
    Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}

#[derive(PartialEq, Clone, Copy)]
enum Page { Overview, Battery, Storage, Crashes, Home, Logs, Network, Security, AllKeys }

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Overview => "Overview", Page::Battery => "Battery", Page::Storage => "Storage", Page::Crashes => "Crashes", Page::Home => "Home screen",
            Page::Logs => "Live log", Page::Network => "Network", Page::Security => "Security", Page::AllKeys => "All values",
        }
    }
}

/// Sidebar groups.
const NAV: [(&str, &[Page]); 3] = [
    ("DEVICE", &[Page::Overview, Page::Battery, Page::Storage, Page::Home]),
    ("INSIGHTS", &[Page::Crashes, Page::Security]),
    ("LIVE", &[Page::Logs, Page::Network, Page::AllKeys]),
];

struct App {
    rt: tokio::runtime::Runtime,
    shared: Arc<Mutex<Shared>>,
    last: Instant,
    page: Page,
    confirm: Option<bool>, // Some(true)=restart, Some(false)=shutdown
    filter: String,
    home: Option<egui::TextureHandle>,
    extras: Option<device::Extras>,
    power: power::Power,
    apps: Job<Vec<apps::AppInfo>>,
    storage: apps::StoragePage,
    crashes: Job<Vec<crashes::Report>>,
    crash_page: crashes::CrashPage,
    logs: logs::Logs,
    net: netcap::Net,
    sec: Job<security::Sec>,
    home_data: Job<home::HomeData>,
    home_page: home::HomePage,
}

impl App {
    /// Home screen is a 2 MB PNG, so it loads once per device and on Refresh, not every 5 s.
    fn refresh_home(&mut self, ctx: &egui::Context) {
        { let mut s = self.shared.lock().unwrap(); if s.home_busy { return; } s.home_busy = true; }
        let (sh, ctx) = (self.shared.clone(), ctx.clone());
        self.rt.spawn(async move {
            let decoded = device::home_screen_png().await.ok().and_then(|png| decode(png, 1000));
            let extras = device::fetch_extras().await.ok();
            let mut s = sh.lock().unwrap();
            s.home_busy = false;
            s.home = decoded;
            s.extras = extras;
            ctx.request_repaint();
        });
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        self.last = Instant::now();
        { let mut s = self.shared.lock().unwrap(); if s.busy { return; } s.busy = true; }
        let (sh, ctx) = (self.shared.clone(), ctx.clone());
        self.rt.spawn(async move {
            let r = device::fetch().await;
            let mut s = sh.lock().unwrap();
            s.busy = false;
            match r { Ok(i) => { s.info = i; s.error = None; } Err(e) => { s.error = Some(e); } }
            ctx.request_repaint();
        });
    }

    /// Drop per-phone data so the next page visit reloads it (unplug, Refresh).
    fn forget(&mut self) {
        self.apps.clear();
        self.crashes.clear();
        self.sec.clear();
        self.home_data.clear();
        self.home_page = Default::default();
    }

    fn power(&mut self, restart: bool) {
        let sh = self.shared.clone();
        self.rt.spawn(async move {
            let r = device::power_action(restart).await;
            sh.lock().unwrap().status = Some(match r { Ok(()) => format!("{} sent", if restart { "Restart" } else { "Shutdown" }), Err(e) => format!("Failed: {e}") });
        });
    }
}

/// One background load. `start` runs the future on the runtime; the UI calls `poll` each frame to pick up the result.
pub struct Job<T> {
    slot: Arc<Mutex<Option<Result<T, String>>>>,
    value: Option<Result<T, String>>,
    running: bool,
}

/// Lets the update loop poll jobs of different types in one list.
trait Poll { fn poll_any(&mut self); }
impl<T: Send + 'static> Poll for Job<T> { fn poll_any(&mut self) { self.poll(); } }

impl<T> Default for Job<T> {
    fn default() -> Self { Self { slot: Default::default(), value: None, running: false } }
}

impl<T: Send + 'static> Job<T> {
    pub fn start(&mut self, rt: &tokio::runtime::Runtime, ctx: &egui::Context, fut: impl std::future::Future<Output = Result<T, String>> + Send + 'static) {
        if self.running { return; }
        self.running = true;
        let (slot, ctx) = (self.slot.clone(), ctx.clone());
        rt.spawn(async move {
            let r = fut.await;
            *slot.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
    }
    pub fn poll(&mut self) {
        if let Some(r) = self.slot.lock().unwrap().take() { self.value = Some(r); self.running = false; }
    }
    pub fn value(&self) -> Option<&Result<T, String>> { self.value.as_ref() }
    pub fn running(&self) -> bool { self.running }
    pub fn clear(&mut self) { self.value = None; }
    /// Start once: when there is no result yet and nothing is running.
    pub fn ensure(&mut self, rt: &tokio::runtime::Runtime, ctx: &egui::Context, fut: impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send>>) {
        if self.value.is_none() && !self.running { self.start(rt, ctx, fut()); }
    }
}

/// %APPDATA%\idev (history, backups). Created on demand.
fn data_dir() -> std::path::PathBuf {
    let d = std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("idev");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Documents\idev (captures, exported reports): files the user opens themselves.
fn docs_dir() -> std::path::PathBuf {
    let d = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join("Documents").join("idev");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Open Explorer on a folder, or with a file selected.
fn reveal(path: &std::path::Path) {
    let arg = if path.is_file() { format!("/select,{}", path.display()) } else { path.display().to_string() };
    let _ = std::process::Command::new("explorer.exe").arg(arg).spawn();
}

/// Local wall-clock stamp for file names, "YYYYMMDD-HHMMSS" (UTC; no timezone crate).
fn stamp() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    format!("{}-{:02}{:02}{:02}", device::civil(s / 86_400).replace('-', ""), s % 86_400 / 3600, s % 3600 / 60, s % 60)
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) {
    egui::Frame::new().fill(card_bg()).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(16))
        .stroke(Stroke::new(1.0, hair()))
        .show(ui, |ui| { ui.set_width(ui.available_width()); add(ui); });
}

fn row(ui: &mut egui::Ui, k: &str, v: impl Into<RichText>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(k).color(muted()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { ui.label(v.into().color(fg())); });
    });
    ui.add_space(6.0);
}

/// Progress ring with a centred label.
fn ring(ui: &mut egui::Ui, frac: f32, color: Color32, top: &str, big: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(84.0, 84.0), Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let r = 36.0;
    p.circle_stroke(c, r, Stroke::new(7.0, soft()));
    let n = 64;
    let pts: Vec<_> = (0..=n).map(|i| {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac.clamp(0.0, 1.0) * i as f32 / n as f32;
        c + vec2(a.cos(), a.sin()) * r
    }).collect();
    p.add(Shape::line(pts, Stroke::new(7.0, color)));
    p.text(c - vec2(0.0, 9.0), Align2::CENTER_CENTER, top, FontId::proportional(11.0), muted());
    p.text(c + vec2(0.0, 8.0), Align2::CENTER_CENTER, big, FontId::proportional(17.0), fg());
}

const BAR_H: f32 = 48.0;

/// Status pill: dot + model + link, light surface with a hairline border.
fn device_chip(ui: &mut egui::Ui, info: Option<&Info>, busy: bool) {
    let (dot, text) = match info {
        Some(i) => (GOOD, format!("{}  ·  {}", device::model_name(&i.s("ProductType")), i.link)),
        None if busy => (WARN, "Searching…".to_string()),
        None => (BAD, "No device".to_string()),
    };
    egui::Frame::new().fill(card_bg()).corner_radius(CornerRadius::same(14))
        .stroke(Stroke::new(1.0, hair())).inner_margin(Margin::symmetric(12, 5))
        // Parent is right-to-left, so the row fills from the right: label first, dot ends up on the left.
        .show(ui, |ui| ui.horizontal(|ui| {
            ui.label(RichText::new(text).size(12.5).color(fg()));
            let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 3.5, dot);
        }));
}

/// Round 28 px button: moon in light mode (switch to dark), sun in dark mode (switch to light).
fn theme_toggle(ui: &mut egui::Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    let p = ui.painter();
    p.circle(rect.center(), 13.5, if resp.hovered() { soft() } else { card_bg() }, Stroke::new(1.0, hair()));
    let c = rect.center();
    if dark() {
        p.circle_filled(c, 4.0, fg());
        for k in 0..8 {
            let a = k as f32 * std::f32::consts::TAU / 8.0;
            let d = vec2(a.cos(), a.sin());
            p.line_segment([c + d * 6.5, c + d * 8.5], Stroke::new(1.3, fg()));
        }
    } else {
        p.circle_filled(c, 6.0, fg());
        p.circle_filled(c + vec2(3.2, -2.6), 5.2, if resp.hovered() { soft() } else { card_bg() }); // bite out of the disc
    }
    resp.on_hover_text(if dark() { "Light mode" } else { "Dark mode" }).clicked()
}

#[derive(Clone, Copy)]
enum Caption { Min, Max, Close }

/// Windows 11 style caption button: 46 px wide, full bar height, glyph drawn with lines; close turns red on hover.
fn caption_button(ui: &mut egui::Ui, kind: Caption, maximized: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(46.0, BAR_H), Sense::click());
    let p = ui.painter();
    let hover = resp.hovered();
    let close = matches!(kind, Caption::Close);
    if hover { p.rect_filled(rect, CornerRadius::ZERO, if close { Color32::from_rgb(0xC4, 0x2B, 0x1C) } else { pick(Color32::from_black_alpha(14), Color32::from_white_alpha(16)) }); }
    let col = if hover && close { Color32::WHITE } else { fg() };
    let s = Stroke::new(1.0, col);
    let c = rect.center();
    match kind {
        Caption::Min => { p.hline(c.x - 5.0..=c.x + 5.0, c.y, s); }
        Caption::Max if maximized => {
            p.rect_stroke(Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(8.0, 8.0)), CornerRadius::same(1), s, egui::StrokeKind::Inside);
            p.line_segment([c + vec2(-2.0, -4.0), c + vec2(4.0, -4.0)], s);
            p.line_segment([c + vec2(4.0, -4.0), c + vec2(4.0, 2.0)], s);
        }
        Caption::Max => { p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), CornerRadius::same(1), s, egui::StrokeKind::Inside); }
        Caption::Close => {
            p.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], s);
            p.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], s);
        }
    }
    resp.clicked()
}

/// Empty bar space drags the window, double-click toggles maximize (the OS title bar is gone).
fn drag_region(ui: &egui::Ui, id: &str) {
    let resp = ui.interact(ui.max_rect(), egui::Id::new(id), Sense::click_and_drag());
    if resp.drag_started_by(egui::PointerButton::Primary) { ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag); }
    if resp.double_clicked() {
        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
}

fn level_color(pct: f32) -> Color32 { if pct >= 0.8 { GOOD } else if pct >= 0.6 { WARN } else { BAD } }

fn gb(b: i64) -> String { format!("{:.1} GB", b as f64 / 1e9) }

/// Phone frame around SpringBoard's own home screen render (it already has the status bar, island, widgets and dock).
fn phone(ui: &mut egui::Ui, home: Option<&egui::TextureHandle>) {
    let (rect, _) = ui.allocate_exact_size(vec2(200.0, 420.0), Sense::hover());
    let p = ui.painter();
    p.add(egui::epaint::Shadow { offset: [0, 12], blur: 30, spread: 0, color: Color32::from_black_alpha(50) }.as_shape(rect, CornerRadius::same(36)));
    p.rect_filled(rect, CornerRadius::same(36), Color32::from_rgb(0x3A, 0x3D, 0x44)); // metal edge
    p.rect_filled(rect.shrink(1.5), CornerRadius::same(35), Color32::BLACK); // bezel
    let screen = rect.shrink(7.0);
    let cr = CornerRadius::same(30);
    match home {
        Some(t) => {
            // Cover-fit: crop to the screen's aspect, centred (the render is native resolution, so this barely trims).
            let (ta, sa) = (t.aspect_ratio(), screen.aspect_ratio());
            let uv = if ta > sa { let u = sa / ta; Rect::from_min_max(pos2((1.0 - u) / 2.0, 0.0), pos2((1.0 + u) / 2.0, 1.0)) }
                     else { let v = ta / sa; Rect::from_min_max(pos2(0.0, (1.0 - v) / 2.0), pos2(1.0, (1.0 + v) / 2.0)) };
            egui::Image::new(egui::load::SizedTexture::from_handle(t)).uv(uv).corner_radius(cr).paint_at(ui, screen);
        }
        None => {
            p.rect_filled(screen, cr, Color32::from_rgb(0x1C, 0x1F, 0x27));
            p.text(screen.center(), Align2::CENTER_CENTER, "Loading home screen…", FontId::proportional(11.0), Color32::from_white_alpha(160));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.last.elapsed() >= REFRESH { self.refresh(ctx); }
        ctx.request_repaint_after(REFRESH);
        let (info, error, busy, status, decoded, home_busy) = {
            let mut s = self.shared.lock().unwrap();
            if let Some(x) = s.extras.take() { self.extras = Some(x); }
            (s.info.clone(), s.error.clone(), s.busy, s.status.clone(), s.home.take(), s.home_busy)
        };
        if let Some(img) = decoded { self.home = Some(ctx.load_texture("home", img, egui::TextureOptions::LINEAR)); }
        match &info {
            None => { // unplugged: forget the old phone
                self.home = None;
                self.extras = None;
                self.forget();
            }
            Some(i) => {
                if self.home.is_none() && !home_busy { self.refresh_home(ctx); }
                self.power.on_info(i);
            }
        }
        for j in [&mut self.apps as &mut dyn Poll, &mut self.crashes, &mut self.sec, &mut self.home_data] { j.poll_any(); }
        // Each page loads its data the first time it is opened (and again after Refresh).
        if info.is_some() {
            let rt = &self.rt;
            match self.page {
                Page::Storage => self.apps.ensure(rt, ctx, || Box::pin(apps::fetch())),
                Page::Crashes => self.crashes.ensure(rt, ctx, || Box::pin(crashes::fetch())),
                Page::Security => {
                    self.sec.ensure(rt, ctx, || Box::pin(security::fetch()));
                    self.apps.ensure(rt, ctx, || Box::pin(apps::fetch()));
                }
                Page::Home => self.home_data.ensure(rt, ctx, || Box::pin(home::fetch())),
                _ => {}
            }
        }
        if self.page != Page::Logs { self.logs.stop(); }

        // Unified chrome: sidebar runs to the top edge (brand lives there), one slim bar over the content.
        egui::SidePanel::left("side").exact_width(220.0).resizable(false).show_separator_line(false)
            .frame(egui::Frame::new().fill(side()).inner_margin(Margin::symmetric(12, 0)))
            .show(ctx, |ui| {
                let r = ui.max_rect().expand2(vec2(12.0, 0.0));
                ui.painter().vline(r.right() - 0.5, r.y_range(), Stroke::new(1.0, hair()));
                let brand = ui.allocate_ui_with_layout(vec2(ui.available_width(), BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(7), ACCENT);
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, "i", FontId::proportional(16.0), Color32::WHITE);
                    ui.add_space(4.0);
                    ui.label(RichText::new("idev").size(15.0).strong().color(fg()));
                });
                drag_region(&ui.new_child(egui::UiBuilder::new().max_rect(brand.response.rect)), "drag_side");
                ui.add_space(10.0);
                ui.label(RichText::new("DEVICE").size(10.5).color(muted()));
                ui.add_space(2.0);
                if let Some(i) = &info {
                    ui.label(RichText::new(device::model_name(&i.s("ProductType"))).strong().size(14.5).color(fg()));
                    ui.label(RichText::new(i.s("DeviceName")).size(12.5).color(muted()));
                } else {
                    ui.label(RichText::new("No device").strong().color(fg()));
                }
                for (group, pages) in NAV {
                    ui.add_space(14.0);
                    ui.label(RichText::new(group).size(10.5).color(muted()));
                    ui.add_space(2.0);
                    for &p in pages {
                        let sel = self.page == p;
                        // Live features show a red dot while they run.
                        let live = (p == Page::Network && self.net.capturing()) || (p == Page::Logs && self.logs.running());
                        let b = egui::Button::new(RichText::new(p.title()).size(13.5).color(if sel { ACCENT } else { fg() }))
                            .fill(if sel { card_bg() } else { Color32::TRANSPARENT })
                            .stroke(if sel { Stroke::new(1.0, hair()) } else { Stroke::NONE })
                            .corner_radius(CornerRadius::same(8)).min_size(vec2(ui.available_width(), 30.0));
                        let r = ui.add(b);
                        if live { ui.painter().circle_filled(r.rect.right_center() - vec2(14.0, 0.0), 3.5, BAD); }
                        if r.clicked() { self.page = p; }
                    }
                }
            });

        egui::TopBottomPanel::top("top").exact_height(BAR_H).show_separator_line(false)
            .frame(egui::Frame::new().fill(bg()).inner_margin(Margin { left: 20, right: 0, top: 0, bottom: 0 }))
            .show(ctx, |ui| {
                drag_region(ui, "drag_top"); // first, so the widgets below win their own clicks
                ui.painter().hline(ui.max_rect().expand2(vec2(20.0, 0.0)).x_range(), ui.max_rect().bottom() - 0.5, Stroke::new(1.0, hair()));
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(self.page.title()).size(16.0).strong().color(fg()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                        if caption_button(ui, Caption::Close, max) { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
                        if caption_button(ui, Caption::Max, max) { ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max)); }
                        if caption_button(ui, Caption::Min, max) { ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)); }
                        ui.add_space(14.0);
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if theme_toggle(ui) {
                            DARK.store(!dark(), std::sync::atomic::Ordering::Relaxed);
                            apply_theme(ctx);
                        }
                        device_chip(ui, info.as_ref(), busy);
                    });
                });
            });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(bg()).inner_margin(Margin::same(18))).show(ctx, |ui| {
            if let Some(e) = &error { ui.colored_label(BAD, e); ui.add_space(8.0); }
            if let Some(s) = &status { ui.colored_label(muted(), s); ui.add_space(8.0); }
            let Some(i) = info else {
                ui.centered_and_justified(|ui| ui.label(RichText::new(if busy { "Looking for an iPhone…" } else { "Plug in an iPhone and tap Trust" }).size(18.0).color(muted())));
                return;
            };
            // The log view scrolls itself (it virtualises 20k rows); every other page scrolls as a whole.
            if self.page == Page::Logs { self.logs.ui(ui, &self.rt); return; }
            egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                Page::Overview => self.overview(ui, &i),
                Page::Battery => self.power.ui(ui, &i),
                Page::Storage => self.storage.ui(ui, &i, &self.apps),
                Page::Crashes => self.crash_page.ui(ui, &self.rt, &self.crashes),
                Page::Home => self.home_page.ui(ui, &self.rt, &mut self.home_data),
                Page::Network => self.net.ui(ui, &self.rt),
                Page::Security => security::ui(ui, &i, self.extras.as_ref(), &self.sec, &self.apps),
                Page::AllKeys => self.all_keys(ui, &i),
                Page::Logs => {}
            });
        });

        if let Some(restart) = self.confirm {
            let what = if restart { "Restart" } else { "Shut down" };
            egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                ui.label(RichText::new(format!("{what} the iPhone?")).size(16.0));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(what).clicked() { self.power(restart); self.confirm = None; }
                    if ui.button("Cancel").clicked() { self.confirm = None; }
                });
            });
        }
    }
}

impl App {
    fn overview(&mut self, ui: &mut egui::Ui, i: &Info) {
        let cap = int_of(&i.disk, "TotalDiskCapacity").map(|b| format!("{} GB", b / 1_000_000_000)).unwrap_or_default();
        card(ui, |ui| ui.horizontal(|ui| {
            ui.label(RichText::new(i.s("DeviceName")).strong().size(15.0));
            ui.add_space(30.0);
            ui.label(format!("{}  ·  {cap}", device::model_name(&i.s("ProductType"))));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let pct = int_of(&i.battery, "BatteryCurrentCapacity").unwrap_or(0);
                let charging = i.battery.get("BatteryIsCharging").and_then(|v| v.as_boolean()).unwrap_or(false);
                ui.label(RichText::new(format!("{}{pct}%", if charging { "Charging  " } else { "" })).color(if charging { GOOD } else { fg() }));
            });
        }));
        ui.add_space(12.0);
        ui.columns(3, |c| {
            c[0].vertical_centered(|ui| {
                phone(ui, self.home.as_ref());
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 214.0 + 4.0).max(0.0) / 2.0 + 4.0);
                    let link = |t: &str| egui::Button::new(RichText::new(t).size(13.5).color(fg())).frame(false).min_size(vec2(68.0, 24.0));
                    if ui.add(link("Restart")).clicked() { self.confirm = Some(true); }
                    if ui.add(link("Shutdown")).clicked() { self.confirm = Some(false); }
                    // The only Refresh in the app: reloads info, home screen and extras.
                    if ui.add(link("Refresh")).clicked() { self.home = None; self.forget(); self.refresh(ui.ctx()); self.refresh_home(ui.ctx()); }
                });
            });
            card(&mut c[1], |ui| {
                row(ui, "iOS version", format!("{} ({})", i.s("ProductVersion"), i.s("BuildVersion")));
                row(ui, "Serial number", i.s("SerialNumber"));
                row(ui, "IMEI 1", i.s("InternationalMobileEquipmentIdentity"));
                row(ui, "IMEI 2", i.s("InternationalMobileEquipmentIdentity2"));
                row(ui, "Model identifier", i.s("ProductType"));
                row(ui, "Model number", format!("{} {}", i.s("ModelNumber"), i.s("RegionInfo")));
                row(ui, "Sales region", device::region_name(&i.s("RegionInfo")));
                let act = i.s("ActivationState");
                row(ui, "Activation", RichText::new(if act == "Activated" { "Yes" } else { &act }).color(if act == "Activated" { fg() } else { WARN }));
                let x = self.extras.as_ref();
                let pending = || RichText::new("…").color(muted());
                row(ui, "Jailbreak (afc2)", match x.and_then(|x| x.afc2) {
                    Some(true) => RichText::new("Signs found").color(WARN), Some(false) => RichText::new("No"), None => pending() });
                row(ui, "iCloud backup", match x.map(|x| x.cloud.get("CloudBackupEnabled").and_then(|v| v.as_boolean())) {
                    Some(Some(true)) => RichText::new("On").color(GOOD), Some(Some(false)) => RichText::new("Off"), Some(None) => RichText::new("Unknown").color(muted()), None => pending() });
                row(ui, "Last iCloud backup", match x.map(|x| int_of(&x.cloud, "LastCloudBackupDate")) {
                    Some(Some(t)) => RichText::new(device::mac_date(t)), Some(None) => RichText::new("Never").color(muted()), None => pending() });
                row(ui, "Crash logs", match x {
                    Some(x) => RichText::new(format!("{} apps  ·  {} reports", x.crashes().len(), x.reports.len())), None => pending() });
                row(ui, "Baseband", i.s("BasebandVersion"));
                row(ui, "UDID", RichText::new(&i.udid).size(11.0));
                ui.add_space(4.0);
                ui.columns(2, |c| {
                    let w = c[0].available_width();
                    let tab = |t: &str| egui::Button::new(RichText::new(t).color(fg())).fill(soft()).corner_radius(CornerRadius::same(8)).min_size(vec2(w, 30.0));
                    if c[0].add(tab("Device details")).clicked() { self.page = Page::AllKeys; }
                    if c[1].add(tab("Battery details")).clicked() { self.page = Page::Battery; }
                });
            });
            let ui = &mut c[2];
            battery_card(ui, i);
            ui.add_space(12.0);
            storage_card(ui, i);
            ui.add_space(12.0);
            checks_card(ui, i, self.extras.as_ref());
            if !i.warnings.is_empty() {
                ui.add_space(12.0);
                card(ui, |ui| for w in &i.warnings { ui.colored_label(WARN, w); });
            }
        });
    }

    fn all_keys(&mut self, ui: &mut egui::Ui, i: &Info) {
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter keys…").desired_width(300.0));
        ui.add_space(10.0);
        let f = self.filter.to_lowercase();
        for (title, d) in [("Lockdown", &i.lockdown), ("Battery", &i.battery), ("Disk", &i.disk), ("Power source (ioreg)", &i.power)] {
            card(ui, |ui| {
                ui.label(RichText::new(title).strong());
                ui.add_space(6.0);
                egui::Grid::new(title).striped(true).num_columns(2).show(ui, |ui| {
                    for (k, v) in d.iter().filter(|(k, _)| f.is_empty() || k.to_lowercase().contains(&f)) {
                        let s = str_of(Some(v));
                        ui.label(RichText::new(k).color(muted()));
                        ui.label(if s.is_empty() { RichText::new("(data)").italics().color(muted()) } else { RichText::new(s) });
                        ui.end_row();
                    }
                });
            });
            ui.add_space(10.0);
        }
    }
}

fn health(i: &Info) -> Option<f32> {
    let (n, d) = (int_of(&i.power, "NominalChargeCapacity")?, int_of(&i.power, "DesignCapacity")?);
    (d > 0).then(|| n as f32 / d as f32)
}

/// Checks built only from what the phone reports. None = still loading / not reported.
fn checks(i: &Info, x: Option<&device::Extras>) -> Vec<(&'static str, Option<bool>, String)> {
    let mut v = Vec::new();
    let h = health(i);
    v.push(("Battery health", h.map(|h| h >= 0.8), h.map_or(String::new(), |h| format!("{:.0}%", h * 100.0))));
    let t = int_of(&i.power, "Temperature").map(|t| t as f32 / 100.0);
    v.push(("Battery temperature", t.map(|t| t < 40.0), t.map_or(String::new(), |t| format!("{t:.0} °C"))));
    let free = int_of(&i.disk, "AmountDataAvailable").zip(int_of(&i.disk, "TotalDataCapacity")).map(|(f, t)| f as f32 / t as f32);
    v.push(("Free storage", free.map(|f| f >= 0.05), free.map_or(String::new(), |f| format!("{:.0}%", f * 100.0))));
    v.push(("Activation", Some(i.s("ActivationState") == "Activated"), String::new()));
    v.push(("No jailbreak (afc2)", x.and_then(|x| x.afc2).map(|j| !j), String::new()));
    let cloud = x.and_then(|x| x.cloud.get("CloudBackupEnabled").and_then(|v| v.as_boolean()));
    v.push(("iCloud backup", cloud, String::new()));
    // App crashes stamped in the last 7 days (file names carry YYYY-MM-DD).
    let now = i.lockdown.get("TimeIntervalSince1970").and_then(|v| v.as_real()).unwrap_or(0.0) as i64;
    let cutoff = device::mac_date(now - 978_307_200 - 7 * 86_400);
    let recent = x.map(|x| x.crashes().iter().filter(|n| date_in(n).is_some_and(|d| d >= cutoff.as_str())).count());
    v.push(("App crashes (7 days)", recent.map(|n| n <= 2), recent.map_or(String::new(), |n| n.to_string())));
    v
}

/// First "YYYY-MM-DD" in a crash report name.
fn date_in(name: &str) -> Option<&str> {
    let b = name.as_bytes();
    (0..b.len().saturating_sub(9)).find(|&k| {
        let s = &b[k..k + 10];
        s[..4].iter().all(u8::is_ascii_digit) && s[4] == b'-' && s[5..7].iter().all(u8::is_ascii_digit) && s[7] == b'-' && s[8..].iter().all(u8::is_ascii_digit)
    }).map(|k| &name[k..k + 10])
}

fn checks_card(ui: &mut egui::Ui, i: &Info, x: Option<&device::Extras>) {
    let list = checks(i, x);
    let bad = list.iter().filter(|c| c.1 == Some(false)).count();
    card(ui, |ui| {
        let (bg, tone, msg) = if bad == 0 { (good_bg(), GOOD, "No issues found".to_string()) }
                            else { (warn_bg(), WARN, format!("{bad} item{} need attention", if bad == 1 { "" } else { "s" })) };
        egui::Frame::new().fill(bg).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| ui.label(RichText::new(msg).color(tone).strong()));
        });
        ui.add_space(8.0);
        for (name, ok, detail) in list {
            let (txt, col) = match ok { Some(true) => ("Normal", GOOD), Some(false) => ("Check", WARN), None => ("…", muted()) };
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).color(fg()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(txt).color(col));
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, col);
                    if !detail.is_empty() { ui.label(RichText::new(detail).color(muted()).size(12.0)); }
                });
            });
            ui.add_space(3.0);
        }
    });
}

fn battery_card(ui: &mut egui::Ui, i: &Info) {
    card(ui, |ui| ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new("Battery health").strong());
            ui.label(RichText::new(format!("Charge cycles  {}", int_of(&i.power, "CycleCount").map_or("?".into(), |c| c.to_string()))).color(muted()));
            if let Some(t) = int_of(&i.power, "Temperature") { ui.label(RichText::new(format!("Temperature  {:.1} °C", t as f32 / 100.0)).color(muted())); }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match health(i) {
            Some(h) => ring(ui, h, level_color(h), "Health", &format!("{:.0}%", h * 100.0)),
            None => { ui.label(RichText::new("n/a").color(muted())); }
        });
    }));
}

fn storage_card(ui: &mut egui::Ui, i: &Info) {
    let total = int_of(&i.disk, "TotalDataCapacity");
    let free = int_of(&i.disk, "AmountDataAvailable");
    card(ui, |ui| ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(format!("Storage  {}", int_of(&i.disk, "TotalDiskCapacity").map_or(String::new(), |b| format!("{} GB", b / 1_000_000_000)))).strong());
            ui.label(RichText::new(format!("Free  {}", free.map_or("?".into(), gb))).color(muted()));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let (Some(t), Some(f)) = (total, free) {
                let frac = f as f32 / t as f32;
                ring(ui, frac, if frac > 0.15 { ACCENT } else { BAD }, "Free", &format!("{:.0}%", frac * 100.0));
            }
        });
    }));
}

fn dump() {
    let rt = tokio::runtime::Runtime::new().expect("tokio");
    match rt.block_on(device::fetch()) {
        Ok(Some(i)) => for (k, d) in [("lockdown", &i.lockdown), ("battery", &i.battery), ("disk", &i.disk), ("power", &i.power)] {
            println!("== {k}");
            for (k, v) in d.iter() { println!("  {k} = {}", str_of(Some(v))); }
        },
        Ok(None) => println!("no device"),
        Err(e) => println!("error: {e}"),
    }
}

/// Frameless windows lose Windows 11's rounded corners; ask DWM for the small radius back.
/// No-op on Windows 10 (DWM ignores the unknown attribute).
fn round_corners(cc: &eframe::CreationContext) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")]
    extern "system" { fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const u32, size: u32) -> i32; }
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUNDSMALL: u32 = 3;
    let Ok(h) = cc.window_handle() else { return };
    if let RawWindowHandle::Win32(w) = h.as_raw() {
        unsafe { DwmSetWindowAttribute(w.hwnd.get(), DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUNDSMALL, 4); }
    }
}

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--dump") { dump(); return Ok(()); }
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1160.0, 760.0]).with_min_inner_size([980.0, 640.0]).with_title("idev").with_decorations(false),
        ..Default::default()
    };
    eframe::run_native("idev", opts, Box::new(|cc| {
        round_corners(cc);
        apply_theme(&cc.egui_ctx);
        let mut app = App {
            rt: tokio::runtime::Runtime::new().expect("tokio"),
            shared: Default::default(),
            last: Instant::now(),
            // `--page storage` opens on a page (handy for shortcuts and screenshots).
            page: std::env::args().skip_while(|a| a != "--page").nth(1)
                .and_then(|w| NAV.iter().flat_map(|g| g.1.iter()).copied().find(|p| p.title().to_lowercase().starts_with(&w.to_lowercase())))
                .unwrap_or(Page::Overview),
            confirm: None,
            filter: String::new(),
            home: None,
            extras: None,
            power: Default::default(),
            apps: Default::default(),
            storage: Default::default(),
            crashes: Default::default(),
            crash_page: Default::default(),
            logs: Default::default(),
            net: Default::default(),
            sec: Default::default(),
            home_data: Default::default(),
            home_page: Default::default(),
        };
        app.refresh(&cc.egui_ctx);
        Ok(Box::new(app))
    }))
}
