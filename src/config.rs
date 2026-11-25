//! Kalıcı ayarlar: `%APPDATA%\Dock\config.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Seffaf,
    Cam,
    Koyu,
}

impl Appearance {
    pub fn index(self) -> i32 {
        match self {
            Appearance::Seffaf => 0,
            Appearance::Cam => 1,
            Appearance::Koyu => 2,
        }
    }

    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Appearance::Cam,
            2 => Appearance::Koyu,
            _ => Appearance::Seffaf,
        }
    }
}

/// Dock'ta sabitlenmiş bir uygulama.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinnedApp {
    pub name: String,
    /// Küçük harfli tam exe yolu (biliniyorsa). Pencere eşleştirmede kullanılır.
    pub exe: Option<String>,
    /// AppUserModelID (biliniyorsa). Pencere eşleştirmede önceliklidir.
    pub aumid: Option<String>,
    /// Başlatma hedefi: exe, .lnk ya da `shell:AppsFolder\<AUMID>`.
    pub launch: String,
    #[serde(default)]
    pub args: String,
    /// İkon kaynağı başlatma hedefinden farklıysa (ör. Chrome profil rozetli .ico).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Bu sabite ait pencerelerde görülen exe yolları (küçük harf). Başlatıcısı
    /// asıl süreçten farklı olan uygulamalar (ör. Discord: Update.exe → Discord.exe)
    /// için arka planda çalışma tespitinde kullanılır.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_exes: Vec<String>,
}

/// Dock'ta gösterilen widget'lar (Ayarlar, 5g).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetsCfg {
    pub music: bool,
    pub weather: bool,
    pub stats: bool,
    /// Ağ, ses ve pil.
    pub system: bool,
    pub clock: bool,
    pub trash: bool,
}

impl Default for WidgetsCfg {
    fn default() -> Self {
        WidgetsCfg { music: true, weather: true, stats: true, system: true, clock: true, trash: true }
    }
}

/// Büyüme oranı aralığı (en büyük ikon / normal ikon).
pub const MAG_MIN: f32 = 1.1;
pub const MAG_MAX: f32 = 1.5;
pub const MAG_DEFAULT: f32 = 1.25;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub auto_hide: bool,
    pub magnify: bool,
    /// Büyüme oranı, `MAG_MIN..=MAG_MAX`.
    pub magnify_max: f32,
    pub widgets: WidgetsCfg,
    /// Dock boyutu ölçeği (1.0 = ikon 44 px), `size_panel::MIN..=MAX` aralığında.
    pub scale: f32,
    pub pinned: Vec<PinnedApp>,
    /// Yığın olarak sabitlenmiş klasörler (5f). Varsayılan: İndirilenler.
    pub stacks: Vec<String>,
    /// Görev çubuğu sabitleri bir kez aktarıldı mı.
    pub pins_imported: bool,
    /// "Windows ile Başlat" varsayılanı (açık) bir kez uygulandı mı.
    pub startup_initialized: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            appearance: Appearance::Seffaf,
            auto_hide: false,
            magnify: true,
            magnify_max: MAG_DEFAULT,
            widgets: WidgetsCfg::default(),
            scale: 1.0,
            pinned: Vec::new(),
            stacks: crate::stacks::downloads_dir().into_iter().collect(),
            pins_imported: false,
            startup_initialized: false,
        }
    }
}

/// `%APPDATA%\Dock`
pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Dock")
}

fn path() -> PathBuf {
    dir().join("config.json")
}

impl Config {
    /// Dosya yoksa ya da bozuksa varsayılan ayarlar.
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            if let Err(e) = std::fs::write(path(), s) {
                eprintln!("[ayarlar] kaydedilemedi: {e}");
            }
        }
    }
}
