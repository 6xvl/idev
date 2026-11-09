//! Everything we read from the phone. Same path as 3uTools: usbmuxd -> lockdownd -> TLS session (pair record) -> services.

use idevice::{
    crashreportcopymobile::CrashReportCopyMobileClient,
    diagnostics_relay::DiagnosticsRelayClient,
    lockdown::LockdownClient,
    provider::UsbmuxdProvider,
    springboardservices::SpringBoardServicesClient,
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection, UsbmuxdDevice},
    IdeviceService,
};
use plist::{Dictionary, Value};

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub udid: String,
    /// "USB" or "Wi-Fi" (usbmuxd lists Wi-Fi phones when Wi-Fi sync is on).
    pub link: &'static str,
    pub lockdown: Dictionary,
    pub battery: Dictionary,
    pub disk: Dictionary,
    /// AppleSmartBattery / IOPMPowerSource ioreg entry (cycles, capacities). Empty when diagnostics_relay refuses.
    pub power: Dictionary,
    pub warnings: Vec<String>,
}

impl Info {
    pub fn s(&self, k: &str) -> String { str_of(self.lockdown.get(k)) }
}

pub fn str_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Integer(i)) => i.to_string(),
        Some(Value::Boolean(b)) => b.to_string(),
        Some(Value::Real(r)) => format!("{r}"),
        _ => String::new(),
    }
}

pub fn int_of(d: &Dictionary, k: &str) -> Option<i64> {
    d.get(k).and_then(|v| v.as_signed_integer().or_else(|| v.as_unsigned_integer().map(|u| u as i64)).or_else(|| v.as_real().map(|r| r as i64)))
}

pub fn dict(v: Value) -> Dictionary { v.into_dictionary().unwrap_or_default() }

fn link_name(c: &Connection) -> &'static str { if matches!(c, Connection::Usb) { "USB" } else { "Wi-Fi" } }

/// The phone to talk to: the first one on a cable, else the first one on Wi-Fi.
pub async fn pick() -> Result<Option<(UsbmuxdConnection, UsbmuxdDevice)>, String> {
    let mut mux = UsbmuxdConnection::default().await.map_err(|e| format!("usbmuxd (Apple Mobile Device Service running?): {e}"))?;
    let mut devs = mux.get_devices().await.map_err(|e| format!("list devices: {e}"))?;
    devs.sort_by_key(|d| !matches!(d.connection_type, Connection::Usb));
    Ok(devs.into_iter().next().map(|d| (mux, d)))
}

pub async fn provider() -> Result<(UsbmuxdProvider, UsbmuxdDevice), String> {
    let (_, dev) = pick().await?.ok_or("no iPhone connected")?;
    Ok((dev.to_provider(UsbmuxdAddr::default(), "idev"), dev))
}

/// Connect to any lockdown service on the current phone (session + TLS handled by the idevice crate).
pub async fn service<S: IdeviceService>() -> Result<S, String> {
    let (pv, _) = provider().await?;
    S::connect(&pv).await.map_err(|e| format!("{}: {e}", S::service_name()))
}

/// Lockdown with a paired session, for domain reads.
pub async fn lockdown() -> Result<LockdownClient, String> {
    let (mut mux, dev) = pick().await?.ok_or("no iPhone connected")?;
    let pair = mux.get_pair_record(&dev.udid).await.map_err(|e| format!("no pair record (trust this PC on the phone): {e}"))?;
    let mut ld = LockdownClient::connect(&dev.to_provider(UsbmuxdAddr::default(), "idev")).await.map_err(|e| format!("lockdownd: {e}"))?;
    ld.start_session(&pair).await.map_err(|e| format!("lockdown session: {e}"))?;
    Ok(ld)
}

pub async fn fetch() -> Result<Option<Info>, String> {
    let Some((_, dev)) = pick().await? else { return Ok(None) };
    let mut ld = lockdown().await?;
    let mut info = Info { udid: dev.udid.clone(), link: link_name(&dev.connection_type), ..Default::default() };
    info.lockdown = dict(ld.get_value(None, None).await.map_err(|e| format!("GetValue: {e}"))?);
    match ld.get_value(None, Some("com.apple.mobile.battery")).await { Ok(v) => info.battery = dict(v), Err(e) => info.warnings.push(format!("battery: {e}")) }
    match ld.get_value(None, Some("com.apple.disk_usage")).await { Ok(v) => info.disk = dict(v), Err(e) => info.warnings.push(format!("disk: {e}")) }

    match service::<DiagnosticsRelayClient>().await {
        Ok(mut dr) => {
            match dr.ioregistry(None, None, Some("IOPMPowerSource")).await {
                Ok(Some(d)) => info.power = d,
                Ok(None) => info.warnings.push("ioregistry: empty".into()),
                Err(e) => info.warnings.push(format!("ioregistry: {e}")),
            }
            let _ = dr.goodbye().await;
        }
        Err(e) => info.warnings.push(e),
    }
    Ok(Some(info))
}

/// SpringBoard's home screen preview. Despite the name ("wallpaper preview") it is the full render at screen
/// resolution (1179x2556 on a 14 Pro): wallpaper, icons, live widget content, status bar, dock, page dots. 3uTools shows this image.
pub async fn home_screen_png() -> Result<Vec<u8>, String> {
    let mut sb = service::<SpringBoardServicesClient>().await?;
    sb.get_home_screen_wallpaper_preview_pngdata().await.map_err(|e| format!("home screen: {e}"))
}

/// Slower facts, fetched once per connection with the home screen (not every 5 s).
#[derive(Clone, Debug, Default)]
pub struct Extras {
    /// com.apple.mobile.backup: CloudBackupEnabled, LastCloudBackupDate (Mac absolute time).
    pub cloud: Dictionary,
    /// com.apple.mobile.wireless_lockdown: EnableWifiConnections, EnableWifiDebugging.
    pub wireless: Dictionary,
    /// lockdownd agreed to start com.apple.afc2 (the classic jailbreak file service). None = check failed.
    pub afc2: Option<bool>,
    /// File names in the crash report folder (.ips: app crashes plus system reports).
    pub reports: Vec<String>,
}

impl Extras {
    /// Real app crashes only, by the same rules as the Crashes page.
    pub fn crashes(&self) -> Vec<&String> {
        self.reports.iter().filter(|n| crate::crashes::kind_of(n) == crate::crashes::Kind::Crash).collect()
    }
}

pub async fn fetch_extras() -> Result<Extras, String> {
    let mut ld = lockdown().await?;
    let mut x = Extras::default();
    if let Ok(v) = ld.get_value(None, Some("com.apple.mobile.backup")).await { x.cloud = dict(v); }
    if let Ok(v) = ld.get_value(None, Some("com.apple.mobile.wireless_lockdown")).await { x.wireless = dict(v); }
    x.afc2 = match ld.start_service("com.apple.afc2").await {
        Ok(_) => Some(true),
        Err(e) if e.to_string().contains("InvalidService") => Some(false),
        Err(_) => None,
    };
    if let Ok(mut c) = service::<CrashReportCopyMobileClient>().await { x.reports = c.ls(None).await.unwrap_or_default(); }
    Ok(x)
}

/// RegionInfo suffix -> sales region. ponytail: common codes only, raw code otherwise.
pub fn region_name(code: &str) -> String {
    let n = match code.trim_end_matches("/A") {
        "LL" => "United States", "J" => "Japan", "B" => "United Kingdom", "C" => "Canada", "X" => "Australia", "ZD" => "Europe",
        "ZA" => "Singapore", "ZP" => "Hong Kong", "CH" => "China", "KH" => "South Korea", "TA" => "Taiwan", "AE" => "UAE / Gulf",
        "HN" => "India", "RU" => "Russia", "TU" => "Turkey", "AB" => "Saudi Arabia / UAE", "FD" => "Europe", "NF" => "France / Belgium",
        "QL" => "Italy / Spain / Portugal", "DN" => "Germany / Austria", "LZ" => "Latin America", "BZ" => "Brazil", "E" => "Mexico",
        _ => return code.to_string(),
    };
    n.to_string()
}

/// Days since 1970-01-01 -> "YYYY-MM-DD" (Howard Hinnant's civil-from-days).
pub fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// "YYYY-MM-DD" -> days since 1970-01-01 (inverse of `civil`).
pub fn days_from_civil(s: &str) -> Option<i64> {
    let mut it = s.get(..10)?.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Mac absolute time (seconds since 2001-01-01 UTC) -> "YYYY-MM-DD".
pub fn mac_date(secs: i64) -> String { civil((secs + 978_307_200).div_euclid(86_400)) }

pub fn today() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64 / 86_400).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mac_date_known_points() {
        assert_eq!(super::mac_date(0), "2001-01-01");
        assert_eq!(super::mac_date(801_611_531), "2026-05-27"); // checked against .NET DateTime
        assert_eq!(super::mac_date(-978_307_200), "1970-01-01");
    }
    #[test]
    fn civil_round_trip() {
        for d in [0, 11_016, 20_766, 20_000, -1, 19_782] { assert_eq!(super::days_from_civil(&super::civil(d)), Some(d)); }
        assert_eq!(super::days_from_civil("2024-02-29"), Some(19_782));
    }
    #[test]
    fn region() { assert_eq!(super::region_name("J/A"), "Japan"); assert_eq!(super::region_name("QQ/A"), "QQ/A"); }
}

pub async fn power_action(restart: bool) -> Result<(), String> {
    let mut dr = service::<DiagnosticsRelayClient>().await?;
    if restart { dr.restart().await } else { dr.shutdown().await }.map_err(|e| e.to_string())
}

/// ProductType -> marketing name. ponytail: recent models only, raw id otherwise; extend the table as needed.
pub fn model_name(pt: &str) -> String {
    let n = match pt {
        "iPhone12,1" => "iPhone 11", "iPhone12,3" => "iPhone 11 Pro", "iPhone12,5" => "iPhone 11 Pro Max", "iPhone12,8" => "iPhone SE (2nd gen)",
        "iPhone13,1" => "iPhone 12 mini", "iPhone13,2" => "iPhone 12", "iPhone13,3" => "iPhone 12 Pro", "iPhone13,4" => "iPhone 12 Pro Max",
        "iPhone14,4" => "iPhone 13 mini", "iPhone14,5" => "iPhone 13", "iPhone14,2" => "iPhone 13 Pro", "iPhone14,3" => "iPhone 13 Pro Max", "iPhone14,6" => "iPhone SE (3rd gen)",
        "iPhone14,7" => "iPhone 14", "iPhone14,8" => "iPhone 14 Plus", "iPhone15,2" => "iPhone 14 Pro", "iPhone15,3" => "iPhone 14 Pro Max",
        "iPhone15,4" => "iPhone 15", "iPhone15,5" => "iPhone 15 Plus", "iPhone16,1" => "iPhone 15 Pro", "iPhone16,2" => "iPhone 15 Pro Max",
        "iPhone17,3" => "iPhone 16", "iPhone17,4" => "iPhone 16 Plus", "iPhone17,1" => "iPhone 16 Pro", "iPhone17,2" => "iPhone 16 Pro Max", "iPhone17,5" => "iPhone 16e",
        _ => return pt.to_string(),
    };
    n.to_string()
}
