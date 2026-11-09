//! Security check: what on this phone widens its attack surface, each with why it matters and how to fix it.

use crate::apps::AppInfo;
use crate::device::{int_of, Extras, Info};
use crate::{card, fg, good_bg, muted, warn_bg, Job, BAD, GOOD, WARN};
use eframe::egui::{self, vec2, CornerRadius, Margin, RichText, Sense};
use idevice::{amfi::AmfiClient, misagent::MisagentClient};

#[derive(Clone, Debug, Default)]
pub struct Sec {
    pub developer_mode: Option<bool>,
    /// Provisioning profile names (developer / enterprise / sideload signing).
    pub profiles: Vec<String>,
}

/// Name out of a signed provisioning profile: the payload is a plain XML plist inside CMS.
pub fn profile_name(raw: &[u8]) -> String {
    let t = String::from_utf8_lossy(raw);
    t.find("<key>Name</key>")
        .and_then(|i| t[i..].find("<string>").map(|j| i + j + 8))
        .and_then(|s| t[s..].find("</string>").map(|e| t[s..s + e].to_string()))
        .unwrap_or_else(|| "(unnamed profile)".into())
}

pub async fn fetch() -> Result<Sec, String> {
    let mut s = Sec::default();
    if let Ok(mut a) = crate::device::service::<AmfiClient>().await { s.developer_mode = a.get_developer_mode_status().await.ok(); }
    let mut m = crate::device::service::<MisagentClient>().await?;
    s.profiles = m.copy_all().await.map_err(|e| format!("misagent: {e}"))?.iter().map(|p| profile_name(p)).collect();
    Ok(s)
}

enum Level { Good, Warn, Info }

struct Item { title: String, level: Level, why: String, fix: Option<&'static str> }

fn items(i: &Info, x: Option<&Extras>, sec: Option<&Sec>, apps: Option<&[AppInfo]>) -> Vec<Item> {
    let mut v = Vec::new();
    let b = |d: &plist::Dictionary, k: &str| d.get(k).and_then(|v| v.as_boolean());

    if let Some(x) = x {
        if b(&x.wireless, "EnableWifiConnections") == Some(true) {
            v.push(Item { title: "Wi-Fi sync is on".into(), level: Level::Warn,
                why: "Any computer this phone trusts can reach it over the local network, not only by cable.".into(),
                fix: Some("Turn off \"Show this iPhone when on Wi-Fi\" in Apple Devices / Finder if you don't use it.") });
        } else {
            v.push(Item { title: "Wi-Fi sync is off".into(), level: Level::Good, why: "Trusted computers need a cable.".into(), fix: None });
        }
        if b(&x.wireless, "EnableWifiDebugging") == Some(true) {
            v.push(Item { title: "Wi-Fi debugging is on".into(), level: Level::Warn, why: "Developer tools can attach over the network.".into(), fix: Some("Disable network debugging in Xcode's Devices window.") });
        }
        match x.afc2 {
            Some(true) => v.push(Item { title: "Jailbreak file service found (afc2)".into(), level: Level::Warn, why: "It exposes the whole filesystem to any trusted computer.".into(), fix: Some("Restore iOS or remove the jailbreak's afc2 package.") }),
            Some(false) => v.push(Item { title: "No jailbreak file service".into(), level: Level::Good, why: "afc2 is absent (newer jailbreaks don't install it, so this is not a full jailbreak check).".into(), fix: None }),
            None => {}
        }
        let now = i.lockdown.get("TimeIntervalSince1970").and_then(|v| v.as_real()).unwrap_or(0.0) as i64 - 978_307_200;
        match (b(&x.cloud, "CloudBackupEnabled"), int_of(&x.cloud, "LastCloudBackupDate")) {
            (Some(true), Some(t)) => {
                let days = (now - t) / 86_400;
                let level = if days > 14 { Level::Warn } else { Level::Good };
                v.push(Item { title: format!("Last iCloud backup {days} days ago ({})", crate::device::mac_date(t)), level,
                    why: if days > 14 { "Backups run on Wi-Fi while charging and locked; a long gap usually means iCloud storage is full.".into() } else { "Recent backup on record.".into() },
                    fix: if days > 14 { Some("Settings › [your name] › iCloud › iCloud Backup › Back Up Now, and check iCloud storage.") } else { None } });
            }
            (Some(false), _) => v.push(Item { title: "iCloud backup is off".into(), level: Level::Warn, why: "Losing the phone loses everything not synced elsewhere.".into(), fix: Some("Settings › [your name] › iCloud › iCloud Backup.") }),
            _ => {}
        }
    }
    if let Some(s) = sec {
        match s.developer_mode {
            Some(true) => v.push(Item { title: "Developer Mode is on".into(), level: Level::Warn, why: "Allows running unsigned development builds and debugging.".into(), fix: Some("Settings › Privacy & Security › Developer Mode, if you don't need it.") }),
            Some(false) => v.push(Item { title: "Developer Mode is off".into(), level: Level::Good, why: "Only App Store and properly signed apps can run.".into(), fix: None }),
            None => {}
        }
        if s.profiles.is_empty() {
            v.push(Item { title: "No provisioning profiles".into(), level: Level::Good, why: "No developer or enterprise signing certificates are trusted.".into(), fix: None });
        } else {
            v.push(Item { title: format!("{} provisioning profile(s): {}", s.profiles.len(), s.profiles.join(", ")), level: Level::Warn,
                why: "These let apps outside the App Store run. Expected if you sideload or develop; suspicious otherwise.".into(),
                fix: Some("Settings › General › VPN & Device Management: remove the ones you don't recognise.") });
        }
    }
    if let Some(apps) = apps {
        let side: Vec<&AppInfo> = apps.iter().filter(|a| a.sideloaded() == Some(true)).collect();
        if side.is_empty() {
            v.push(Item { title: "All apps come from the App Store".into(), level: Level::Good, why: format!("{} user apps, all Apple-signed.", apps.len()), fix: None });
        } else {
            let names: Vec<String> = side.iter().map(|a| a.name.clone()).collect();
            v.push(Item { title: format!("{} app(s) installed outside the App Store", side.len()), level: Level::Warn,
                why: format!("{}. They skip App Review.", names.join(", ")), fix: Some("Delete any you don't recognise.") });
        }
    }
    v.push(Item { title: "Not covered".into(), level: Level::Info,
        why: "Configuration (MDM) profiles and Find My status aren't readable over this connection on iOS 26. Check Settings › General › VPN & Device Management.".into(), fix: None });
    v
}

pub fn ui(ui: &mut egui::Ui, i: &Info, x: Option<&Extras>, sec: &Job<Sec>, apps: &Job<Vec<AppInfo>>) {
    let s = sec.value().and_then(|r| r.as_ref().ok());
    let a = apps.value().and_then(|r| r.as_ref().ok()).map(Vec::as_slice);
    let list = items(i, x, s, a);
    let warn = list.iter().filter(|it| matches!(it.level, Level::Warn)).count();
    card(ui, |ui| {
        let (bg, tone, msg) = if warn == 0 { (good_bg(), GOOD, "Nothing risky found".to_string()) } else { (warn_bg(), WARN, format!("{warn} thing{} worth a look", if warn == 1 { "" } else { "s" })) };
        egui::Frame::new().fill(bg).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(msg).strong().color(tone));
        });
        if let Some(Err(e)) = sec.value() { ui.colored_label(BAD, e); }
        if sec.running() || apps.running() { ui.label(RichText::new("Still checking profiles and apps…").size(12.0).color(muted())); }
        ui.add_space(8.0);
        for it in &list {
            let col = match it.level { Level::Good => GOOD, Level::Warn => WARN, Level::Info => muted() };
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 18.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, col);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&it.title).strong().color(fg()));
                    ui.label(RichText::new(&it.why).size(12.5).color(muted()));
                    if let Some(f) = it.fix { ui.label(RichText::new(format!("Fix: {f}")).size(12.5).color(fg())); }
                });
            });
            ui.add_space(8.0);
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_from_profile() {
        let raw = b"\x30\x80junk<?xml?><plist><dict><key>AppIDName</key><string>X</string><key>Name</key>\n\t<string>iOS Team Provisioning Profile: *</string></dict>";
        assert_eq!(super::profile_name(raw), "iOS Team Provisioning Profile: *");
        assert_eq!(super::profile_name(b"nothing"), "(unnamed profile)");
    }
}
