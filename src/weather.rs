//! Open-Meteo'dan Ankara için anlık hava durumu (WinHTTP, ek crate yok).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Networking::WinHttp::*;

const HOST: &str = "api.open-meteo.com";
const PATH: &str = "/v1/forecast?latitude=39.93&longitude=32.86&current=temperature_2m,weather_code,is_day&timezone=auto";
const REFRESH: Duration = Duration::from_secs(30 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);

/// Anlık hava durumu.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weather {
    pub temp_c: f32,
    /// Open-Meteo WMO kodu.
    pub code: u8,
    pub is_day: bool,
    /// Kısa Türkçe açıklama.
    pub description: String,
    /// Başarılı çekimin unix zamanı (sn).
    pub fetched_unix: u64,
}

impl Weather {
    /// Arayüzün kullandığı ikon adı.
    pub fn icon(&self) -> &'static str {
        icon_for(self.code, self.is_day)
    }
}

/// WMO kodu -> Türkçe açıklama.
fn describe(code: u8) -> &'static str {
    match code {
        0 => "Açık",
        1 => "Az bulutlu",
        2 => "Parçalı bulutlu",
        3 => "Kapalı",
        45 | 48 => "Sisli",
        51 | 53 | 55 => "Çiseleme",
        56 | 57 => "Dondurucu çiseleme",
        61 | 63 | 65 => "Yağmurlu",
        66 | 67 => "Dondurucu yağmur",
        71 | 73 | 75 | 77 => "Karlı",
        80..=82 => "Sağanak",
        85 | 86 => "Kar sağanağı",
        95 | 96 | 99 => "Gök gürültülü fırtına",
        _ => "Bilinmiyor",
    }
}

/// WMO kodu + gece/gündüz -> ikon adı.
fn icon_for(code: u8, is_day: bool) -> &'static str {
    match code {
        0 => {
            if is_day {
                "clear-day"
            } else {
                "clear-night"
            }
        }
        1 | 2 => {
            if is_day {
                "partly-day"
            } else {
                "partly-night"
            }
        }
        3 => "cloudy",
        45 | 48 => "fog",
        51 | 53 | 55 | 56 | 57 => "drizzle",
        61 | 63 | 65 | 66 | 67 | 80..=82 => "rain",
        71 | 73 | 75 | 77 | 85 | 86 => "snow",
        95 | 96 | 99 => "thunder",
        _ => "cloudy",
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("Dock").join("weather.json"))
}

fn load_cache() -> Option<Weather> {
    let data = std::fs::read(cache_path()?).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_cache(w: &Weather) {
    let Some(path) = cache_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(w) {
        let _ = std::fs::write(path, json);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Açık WinHTTP tutamaçlarını Drop'ta kapatır.
struct Handle(*mut core::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

/// HTTPS GET; 200 ise gövdeyi döndürür.
fn http_get(host: &str, path: &str) -> Option<Vec<u8>> {
    unsafe {
        let agent = wide("Dock/0.1");
        let session = Handle(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        ));
        if session.0.is_null() {
            return None;
        }
        // çözümleme, bağlantı, gönderme, alma (ms)
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 10_000, 10_000);

        let whost = wide(host);
        let conn = Handle(WinHttpConnect(
            session.0,
            whost.as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if conn.0.is_null() {
            return None;
        }

        let verb = wide("GET");
        let wpath = wide(path);
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            verb.as_ptr(),
            wpath.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return None;
        }

        if WinHttpSendRequest(req.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 {
            return None;
        }
        if WinHttpReceiveResponse(req.0, std::ptr::null_mut()) == 0 {
            return None;
        }

        // Durum kodu
        let mut status: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let ok = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut core::ffi::c_void,
            &mut size,
            std::ptr::null_mut(),
        );
        if ok == 0 || status != 200 {
            return None;
        }

        // Gövde (en fazla 1 MB)
        let mut body = Vec::new();
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(req.0, &mut avail) == 0 {
                return None;
            }
            if avail == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut read: u32 = 0;
            if WinHttpReadData(
                req.0,
                body[start..].as_mut_ptr() as *mut core::ffi::c_void,
                avail,
                &mut read,
            ) == 0
            {
                return None;
            }
            body.truncate(start + read as usize);
            if body.len() > 1_000_000 {
                return None;
            }
        }
        Some(body)
    }
}

#[derive(Deserialize)]
struct ApiResponse {
    current: ApiCurrent,
}

#[derive(Deserialize)]
struct ApiCurrent {
    temperature_2m: f32,
    weather_code: u8,
    is_day: u8,
}

fn parse(body: &[u8]) -> Option<Weather> {
    let r: ApiResponse = serde_json::from_slice(body).ok()?;
    Some(Weather {
        temp_c: r.current.temperature_2m,
        code: r.current.weather_code,
        is_day: r.current.is_day != 0,
        description: describe(r.current.weather_code).to_string(),
        fetched_unix: now_unix(),
    })
}

/// Tek seferlik çekim; hata olursa `None`.
pub fn fetch_now() -> Option<Weather> {
    parse(&http_get(HOST, PATH)?)
}

/// Arka plan iş parçacığı başlatır: önce önbellek, sonra 30 dk'da bir
/// çekim (hata olursa 5 dk sonra yeniden dener).
pub fn start(on_update: impl Fn(Weather) + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("weather".into())
        .spawn(move || {
            if let Some(w) = load_cache() {
                on_update(w);
            }
            loop {
                let wait = match fetch_now() {
                    Some(w) => {
                        save_cache(&w);
                        on_update(w);
                        REFRESH
                    }
                    None => RETRY,
                };
                std::thread::sleep(wait);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(code: u8, is_day: bool) -> Weather {
        Weather {
            temp_c: 10.0,
            code,
            is_day,
            description: describe(code).to_string(),
            fetched_unix: 0,
        }
    }

    #[test]
    fn mapping() {
        let cases: &[(u8, &str, &str, &str)] = &[
            (0, "Açık", "clear-day", "clear-night"),
            (1, "Az bulutlu", "partly-day", "partly-night"),
            (2, "Parçalı bulutlu", "partly-day", "partly-night"),
            (3, "Kapalı", "cloudy", "cloudy"),
            (45, "Sisli", "fog", "fog"),
            (48, "Sisli", "fog", "fog"),
            (51, "Çiseleme", "drizzle", "drizzle"),
            (53, "Çiseleme", "drizzle", "drizzle"),
            (55, "Çiseleme", "drizzle", "drizzle"),
            (56, "Dondurucu çiseleme", "drizzle", "drizzle"),
            (57, "Dondurucu çiseleme", "drizzle", "drizzle"),
            (61, "Yağmurlu", "rain", "rain"),
            (63, "Yağmurlu", "rain", "rain"),
            (65, "Yağmurlu", "rain", "rain"),
            (66, "Dondurucu yağmur", "rain", "rain"),
            (67, "Dondurucu yağmur", "rain", "rain"),
            (71, "Karlı", "snow", "snow"),
            (73, "Karlı", "snow", "snow"),
            (75, "Karlı", "snow", "snow"),
            (77, "Karlı", "snow", "snow"),
            (80, "Sağanak", "rain", "rain"),
            (81, "Sağanak", "rain", "rain"),
            (82, "Sağanak", "rain", "rain"),
            (85, "Kar sağanağı", "snow", "snow"),
            (86, "Kar sağanağı", "snow", "snow"),
            (95, "Gök gürültülü fırtına", "thunder", "thunder"),
            (96, "Gök gürültülü fırtına", "thunder", "thunder"),
            (99, "Gök gürültülü fırtına", "thunder", "thunder"),
        ];
        for &(code, desc, day, night) in cases {
            assert_eq!(describe(code), desc, "code {code}");
            assert_eq!(w(code, true).icon(), day, "code {code} day");
            assert_eq!(w(code, false).icon(), night, "code {code} night");
        }
    }

    #[test]
    fn parse_sample() {
        let body = br#"{"latitude":39.93,"current":{"time":"x","interval":900,"temperature_2m":7.4,"weather_code":3,"is_day":0}}"#;
        let p = parse(body).unwrap();
        assert_eq!(p.code, 3);
        assert!(!p.is_day);
        assert_eq!(p.icon(), "cloudy");
        assert!(parse(b"garbage").is_none());
    }
}
