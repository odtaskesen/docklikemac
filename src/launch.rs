//! Kabuk yardımcıları: kısayol çözme, görünen ad, başlatma, kayıt defteri ayarları.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::ptr::{null, null_mut};

use windows::core::{Interface, PCWSTR};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    IShellItem, IShellLinkW, SHCreateItemFromParsingName, ShellLink, SIGDN_NORMALDISPLAY,
};

use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_SZ,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Çözülmüş kısayol bilgisi.
pub struct LinkInfo {
    pub target: Option<String>,
    /// Dock kısayolun kendisini başlattığı için şimdilik kullanılmıyor.
    #[allow(dead_code)]
    pub args: String,
    pub aumid: Option<String>,
}

const APPS_FOLDER_PREFIX: &str = "shell:appsfolder\\";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// `...\windowsapps\<Ad>_<Sürüm>_<Mimari>_<Kaynak>_<Yayıncı>\...` yolundaki paket
/// klasörünün parçaları: (ad, yayıncı kimliği, klasörden sonrası).
pub fn windowsapps_parts(exe: &str) -> Option<(String, String, String)> {
    let lower = exe.to_lowercase();
    let i = lower.find("\\windowsapps\\")? + "\\windowsapps\\".len();
    let rest = &lower[i..];
    let (folder, tail) = rest.split_once('\\').unwrap_or((rest, ""));
    let parts: Vec<&str> = folder.split('_').collect();
    if parts.len() < 5 {
        return None;
    }
    Some((parts[0].to_string(), parts[parts.len() - 1].to_string(), tail.to_string()))
}

/// Store uygulamasının exe yolundan sürümden bağımsız AppUserModelID'si
/// (ör. `OpenAI.Codex_2p2nqsd0c76g0!App`). Paket kuruluysa çalışır; uygulama
/// sürümü güncellenip eski klasör silinmiş olsa da aile adıyla bulunur.
pub fn packaged_aumid(exe: &str) -> Option<String> {
    use windows_sys::Win32::Storage::Packaging::Appx::{
        ClosePackageInfo, GetPackageApplicationIds, GetPackagesByPackageFamily,
        OpenPackageInfoByFullName, _PACKAGE_INFO_REFERENCE,
    };
    let (name, publisher, _) = windowsapps_parts(exe)?;
    let family = wide(&format!("{name}_{publisher}"));
    // SAFETY: tamponlar API'nin bildirdiği boyutlarda ayrılır; paket bilgisi kapatılır.
    unsafe {
        let (mut count, mut len) = (0u32, 0u32);
        GetPackagesByPackageFamily(family.as_ptr(), &mut count, null_mut(), &mut len, null_mut());
        if count == 0 || len == 0 {
            return None;
        }
        let mut names: Vec<*mut u16> = vec![null_mut(); count as usize];
        let mut buf = vec![0u16; len as usize];
        if GetPackagesByPackageFamily(family.as_ptr(), &mut count, names.as_mut_ptr(), &mut len, buf.as_mut_ptr()) != 0 {
            return None;
        }
        let mut info: *mut _PACKAGE_INFO_REFERENCE = null_mut();
        if OpenPackageInfoByFullName(names[0], 0, &mut info) != 0 || info.is_null() {
            return None;
        }
        let (mut blen, mut n) = (0u32, 0u32);
        GetPackageApplicationIds(info, &mut blen, null_mut(), &mut n);
        let mut ids = vec![0u8; blen as usize];
        let rc = GetPackageApplicationIds(info, &mut blen, ids.as_mut_ptr(), &mut n);
        ClosePackageInfo(info);
        if rc != 0 || n == 0 {
            return None;
        }
        // Tampon: n adet PCWSTR işaretçisi, ardından dizgeler.
        // Bayt tamponu hizalı olmayabilir: işaretçiyi hizasız oku.
        let first = std::ptr::read_unaligned(ids.as_ptr() as *const *const u16);
        PCWSTR(first).to_string().ok()
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide_buf(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..len]).to_string_lossy().into_owned()
}

fn is_apps_folder(s: &str) -> bool {
    s.to_lowercase().starts_with(APPS_FOLDER_PREFIX)
}

fn ext_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn stem_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// .lnk dosyasını çözer: hedef, argümanlar ve AppUserModelID.
pub fn resolve_link(lnk_path: &str) -> Option<LinkInfo> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        let w = wide(lnk_path);
        persist.Load(PCWSTR(w.as_ptr()), STGM_READ).ok()?;

        let mut buf = [0u16; 1024];
        let target = if link.GetPath(&mut buf, null_mut(), 0).is_ok() {
            let s = from_wide_buf(&buf);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        } else {
            None
        };

        let mut abuf = [0u16; 2048];
        let args = if link.GetArguments(&mut abuf).is_ok() {
            from_wide_buf(&abuf)
        } else {
            String::new()
        };

        let aumid = link.cast::<IPropertyStore>().ok().and_then(|store| {
            let pv = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
            let p = PropVariantToStringAlloc(&pv).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s.filter(|s| !s.is_empty())
        });

        Some(LinkInfo { target, args, aumid })
    }
}

/// Okunabilir uygulama adı.
pub fn display_name(parsing_name: &str) -> String {
    if is_apps_folder(parsing_name) {
        return shell_display_name(parsing_name).unwrap_or_else(|| parsing_name.to_string());
    }
    match ext_of(parsing_name).as_str() {
        "exe" => file_description(parsing_name).unwrap_or_else(|| stem_of(parsing_name)),
        _ => stem_of(parsing_name),
    }
}

/// IShellItem görünen adı.
fn shell_display_name(parsing_name: &str) -> Option<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let w = wide(parsing_name);
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        let p = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

/// Sürüm bilgisinden FileDescription (ilk çeviri tablosu).
fn file_description(path: &str) -> Option<String> {
    unsafe {
        let w = wide(path);
        let mut handle = 0u32;
        let size = GetFileVersionInfoSizeW(w.as_ptr(), &mut handle);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(w.as_ptr(), 0, size, data.as_mut_ptr() as *mut _) == 0 {
            return None;
        }
        let mut ptr: *mut core::ffi::c_void = null_mut();
        let mut len = 0u32;
        let tr = wide("\\VarFileInfo\\Translation");
        if VerQueryValueW(data.as_ptr() as *const _, tr.as_ptr(), &mut ptr, &mut len) == 0
            || ptr.is_null()
            || len < 4
        {
            return None;
        }
        let pair = std::slice::from_raw_parts(ptr as *const u16, 2);
        let sub = wide(&format!(
            "\\StringFileInfo\\{:04x}{:04x}\\FileDescription",
            pair[0], pair[1]
        ));
        let mut sptr: *mut core::ffi::c_void = null_mut();
        let mut slen = 0u32;
        if VerQueryValueW(data.as_ptr() as *const _, sub.as_ptr(), &mut sptr, &mut slen) == 0
            || sptr.is_null()
            || slen == 0
        {
            return None;
        }
        let chars = std::slice::from_raw_parts(sptr as *const u16, slen as usize);
        let s = from_wide_buf(chars).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// ShellExecuteW "open" ile başlatır.
pub fn launch(target: &str, args: &str) -> bool {
    let t = wide(target);
    let a = wide(args);
    let verb = wide("open");
    let r = unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            t.as_ptr(),
            if args.is_empty() { null() } else { a.as_ptr() },
            null(),
            SW_SHOWNORMAL,
        )
    };
    (r as isize) > 32
}

/// Explorer'da dosyayı seçili açar (AppsFolder için yalnızca klasörü açar).
pub fn show_in_folder(path: &str) {
    if is_apps_folder(path) {
        launch("shell:AppsFolder", "");
    } else {
        launch("explorer.exe", &format!("/select,\"{}\"", path));
    }
}

/// HKCU altındaki anahtarı açar.
fn open_key(sub: &str, access: u32) -> Option<HKEY> {
    let w = wide(sub);
    let mut key: HKEY = null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, w.as_ptr(), 0, access, &mut key) };
    if rc == 0 {
        Some(key)
    } else {
        None
    }
}

/// Windows vurgu rengi, 0xRRGGBB.
pub fn accent_color() -> u32 {
    const FALLBACK: u32 = 0x0078D4;
    let Some(key) = open_key("Software\\Microsoft\\Windows\\DWM", KEY_READ) else {
        return FALLBACK;
    };
    let name = wide("AccentColor");
    let mut ty = 0u32;
    let mut val = 0u32;
    let mut sz = 4u32;
    let rc = unsafe {
        let rc = RegQueryValueExW(
            key,
            name.as_ptr(),
            null(),
            &mut ty,
            &mut val as *mut u32 as *mut u8,
            &mut sz,
        );
        RegCloseKey(key);
        rc
    };
    if rc != 0 || ty != REG_DWORD {
        return FALLBACK;
    }
    // ABGR -> RRGGBB
    let (r, g, b) = (val & 0xFF, (val >> 8) & 0xFF, (val >> 16) & 0xFF);
    (r << 16) | (g << 8) | b
}

/// Run anahtarında değer var mı.
pub fn startup_enabled(value_name: &str) -> bool {
    let Some(key) = open_key(RUN_KEY, KEY_READ) else {
        return false;
    };
    let name = wide(value_name);
    let rc = unsafe {
        let rc = RegQueryValueExW(key, name.as_ptr(), null(), null_mut(), null_mut(), null_mut());
        RegCloseKey(key);
        rc
    };
    rc == 0
}

/// Oturum açılışında çalıştırmayı açar/kapatır.
pub fn set_startup(value_name: &str, command_line: &str, on: bool) -> bool {
    let Some(key) = open_key(RUN_KEY, KEY_SET_VALUE) else {
        return false;
    };
    let name = wide(value_name);
    let rc = unsafe {
        let rc = if on {
            let v = wide(command_line);
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                v.as_ptr() as *const u8,
                (v.len() * 2) as u32,
            )
        } else {
            let rc = RegDeleteValueW(key, name.as_ptr());
            // 2 = ERROR_FILE_NOT_FOUND: zaten yok, başarı sayılır.
            if rc == 2 {
                0
            } else {
                rc
            }
        };
        RegCloseKey(key);
        rc
    };
    rc == 0
}

/// Görev çubuğuna sabitlenmiş uygulamaların .lnk yolları.
pub fn taskbar_pins() -> Vec<String> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let dir = std::path::Path::new(&appdata)
        .join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")))
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

