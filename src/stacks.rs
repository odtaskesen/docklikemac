//! Yığınlar (macOS Stacks): sabitlenmiş klasörün içeriği ve ikonları.
//! Sadece veri ve ikon işi yapar; çizim başka modülde.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Downloads, IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName,
    SHGetKnownFolderPath, KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY, SIIGBF_ICONONLY,
    SIIGBF_RESIZETOFIT,
};
use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
    FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Klasördeki bir öğe.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Tam yol.
    pub path: String,
    /// Görünen ad: dosya adı (uzantısıyla).
    pub name: String,
    /// Eklenme zamanı: oluşturma zamanı (FILETIME, 100 ns), sıralama için.
    pub time: u64,
}

/// Premultiplied BGRA, yukarıdan aşağıya, len == w*h*4. GDI AlphaBlend (AC_SRC_ALPHA) için hazır.
pub struct Bitmap {
    pub w: i32,
    pub h: i32,
    pub bgra: Vec<u8>,
}

/// UTF-16, sonu NUL.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// NUL'a kadar olan UTF-16 diliminden String.
fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Kullanıcının İndirilenler klasörü. Yoksa None.
pub fn downloads_dir() -> Option<String> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

/// Yolun son bileşeni; yoksa yolun kendisi.
fn last_component(dir: &str) -> String {
    let t = dir.trim_end_matches(['\\', '/']);
    t.rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(dir)
        .to_string()
}

/// Klasörün kabuk görünen adı (ör. "İndirilenler"). Başarısızsa yolun son bileşeni.
pub fn display_name(dir: &str) -> String {
    crate::icons::com_init_thread();
    let w = wide(dir);
    let name = unsafe {
        (|| {
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
            let p = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        })()
    };
    name.filter(|s| !s.is_empty())
        .unwrap_or_else(|| last_component(dir))
}

/// Yarım indirme uzantısı mı (.crdownload, .part, .tmp)?
fn is_partial(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".crdownload", ".part", ".tmp"]
        .iter()
        .any(|e| lower.ends_with(e))
}

fn filetime(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// Klasörün doğrudan içeriği, en yeni önce (eşitse ada göre, büyük/küçük harf duyarsız).
/// En fazla `limit` öğe döner (sıralamadan sonra kesilir). Okunamazsa boş.
pub fn list(dir: &str, limit: usize) -> Vec<Entry> {
    let base = dir.trim_end_matches(['\\', '/']);
    let pattern = wide(&format!("{base}\\*"));
    let mut out: Vec<Entry> = Vec::new();
    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let h = FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        );
        if h == INVALID_HANDLE_VALUE {
            return out;
        }
        loop {
            let name = from_wide(&data.cFileName);
            let attrs = data.dwFileAttributes;
            let skip = name == "."
                || name == ".."
                || attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
                || name.eq_ignore_ascii_case("desktop.ini")
                || is_partial(&name);
            if !skip {
                out.push(Entry {
                    path: format!("{base}\\{name}"),
                    name,
                    time: filetime(&data.ftCreationTime),
                });
            }
            if FindNextFileW(h, &mut data) == 0 {
                break;
            }
        }
        FindClose(h);
    }
    out.sort_by(|a, b| {
        b.time
            .cmp(&a.time)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out.truncate(limit);
    out
}

/// HBITMAP'i her yolda serbest bırakan koruyucu.
struct BitmapGuard(HBITMAP);

impl Drop for BitmapGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}

/// HBITMAP'i premultiplied BGRA'ya çevirir.
fn to_bitmap(hbmp: HBITMAP) -> Option<Bitmap> {
    unsafe {
        let mut bm: BITMAP = std::mem::zeroed();
        let got = GetObjectW(
            HGDIOBJ(hbmp.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        );
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut bgra = vec![0u8; (w as usize) * (h as usize) * 4];
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // yukarıdan aşağıya
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let dc = GetDC(Some(HWND(std::ptr::null_mut())));
        let lines = GetDIBits(
            dc,
            hbmp,
            0,
            h as u32,
            Some(bgra.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(Some(HWND(std::ptr::null_mut())), dc);
        if lines == 0 {
            return None;
        }
        if bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            // Alfa kanalı yok: opak kabul et.
            for p in bgra.as_chunks_mut::<4>().0.iter_mut() {
                p[3] = 255;
            }
        } else {
            // Veri premultiplied gelir; güvenlik için renk kanalını alfaya kırp.
            for p in bgra.as_chunks_mut::<4>().0.iter_mut() {
                let a = p[3];
                p[0] = p[0].min(a);
                p[1] = p[1].min(a);
                p[2] = p[2].min(a);
            }
        }
        Some(Bitmap { w, h, bgra })
    }
}

/// Tek bir yol için ikon/küçük resim yükler.
fn load_one(path: &str, size: i32) -> Option<Bitmap> {
    let w = wide(path);
    let factory: IShellItemImageFactory =
        unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()? };
    let sz = windows::Win32::Foundation::SIZE { cx: size, cy: size };
    let first = unsafe { factory.GetImage(sz, SIIGBF_RESIZETOFIT) };
    let hbmp = match first {
        Ok(h) => h,
        Err(_) => unsafe { factory.GetImage(sz, SIIGBF_ICONONLY).ok()? },
    };
    let _guard = BitmapGuard(hbmp);
    to_bitmap(hbmp)
}

/// `paths` için ikonları/küçük resimleri "stack-icons" iş parçacığında sırayla yükler.
/// Her başarılı ikon için `on_icon(index_in_paths, bitmap)` çağrılır (iş parçacığından).
/// Dönen bayrak true yapılırsa iş parçacığı bir sonraki öğeden önce biter.
pub fn load_icons(
    paths: Vec<String>,
    size: i32,
    on_icon: impl Fn(usize, Bitmap) + Send + 'static,
) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let spawned = std::thread::Builder::new()
        .name("stack-icons".into())
        .spawn(move || {
            crate::icons::com_init_thread();
            for (i, p) in paths.iter().enumerate() {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(bmp) = load_one(p, size) {
                    on_icon(i, bmp);
                }
            }
        });
    if spawned.is_err() {
        cancel.store(true, Ordering::Relaxed);
    }
    cancel
}

/// Klasörü ya da dosyayı varsayılan uygulamayla açar. Klasör Gezgin'de açılır.
pub fn open(path: &str) {
    let verb = wide("open");
    let file = wide(path);
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}
