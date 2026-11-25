//! Geri Dönüşüm Kutusu: boş/dolu izleme, açma, boşaltma, dosya atma.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_SIZE,
    FindFirstChangeNotificationW, FindNextChangeNotification,
    GetDriveTypeW, GetLogicalDrives,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, WaitForMultipleObjects};
use windows_sys::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_WANTNUKEWARNING, SHEmptyRecycleBinW,
    SHFileOperationW, SHFILEOPSTRUCTW, SHQUERYRBINFO, SHQueryRecycleBinW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const WAIT_TIMEOUT: u32 = 0x102;
/// Güvenlik amaçlı yeniden sorgu aralığı.
const SAFETY: Duration = Duration::from_secs(300);
/// Değişiklik sonrası sessizlik süresi.
const DEBOUNCE_MS: u32 = 300;
/// İzleyici kurulamazsa yoklama aralığı.
const POLL: Duration = Duration::from_secs(10);

/// NUL sonlu geniş dize.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Arka plan iş parçacığında Geri Dönüşüm Kutusu'nu izler. `on_change(full)` bir kez
/// hemen (başlangıç durumu) ve sonra yalnızca boş/dolu durumu değişince çağrılır
/// (izleyici iş parçacığından). Bir kez çağrılır.
pub fn start(on_change: impl Fn(bool) + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("recycle".into())
        .spawn(move || watch(on_change));
}

/// Şu anki durum (SHQueryRecycleBinW, tüm sürücüler). Engelleyebilir; UI iş parçacığında çağrılmayacak.
pub fn is_full() -> bool {
    let mut info = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        i64Size: 0,
        i64NumItems: 0,
    };
    // SAFETY: info geçerli, cbSize ayarlı; kök yolu null (tüm sürücüler).
    let hr = unsafe { SHQueryRecycleBinW(null(), &mut info) };
    hr >= 0 && info.i64NumItems > 0
}

/// Geri Dönüşüm Kutusu klasörünü Gezgin'de açar.
pub fn open() {
    let verb = wide("open");
    let exe = wide("explorer.exe");
    let arg = wide("shell:RecycleBinFolder");
    // SAFETY: tüm dizeler NUL sonlu ve çağrı boyunca canlı.
    unsafe {
        ShellExecuteW(null_mut(), verb.as_ptr(), exe.as_ptr(), arg.as_ptr(), null(), SW_SHOWNORMAL);
    }
}

/// Boşaltır. Windows'un kendi onay penceresi çıkar; modal olduğu için ayrı iş parçacığında.
pub fn empty() {
    let _ = std::thread::Builder::new().name("recycle-empty".into()).spawn(|| {
        // SAFETY: hwnd ve kök yolu null, bayrak 0 (Windows onay sorar).
        unsafe {
            SHEmptyRecycleBinW(null_mut(), null(), 0);
        }
    });
}

/// Dosyaları/klasörleri Geri Dönüşüm Kutusu'na taşır. Ayrı iş parçacığında, hemen döner.
pub fn recycle(paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("recycle-del".into()).spawn(move || {
        // pFrom: her yol NUL ile biter, liste çift NUL ile.
        let mut from: Vec<u16> = Vec::new();
        for p in &paths {
            from.extend(p.encode_utf16());
            from.push(0);
        }
        from.push(0);
        let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
        op.wFunc = FO_DELETE;
        op.pFrom = from.as_ptr();
        op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING) as u16;
        // SAFETY: op geçerli; from çağrı boyunca canlı ve çift NUL sonlu.
        unsafe {
            SHFileOperationW(&mut op);
        }
    });
}

/// Geçerli kullanıcının SID dizesi.
fn user_sid() -> Option<String> {
    // SAFETY: token tutamacı açılır, iş bitince kapatılır; tampon TOKEN_USER için yeterince büyük.
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut buf = vec![0u64; 128]; // 1 KB, hizalı
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr() as *mut _,
            (buf.len() * 8) as u32,
            &mut len,
        );
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s: *mut u16 = null_mut();
        if ConvertSidToStringSidW(tu.User.Sid, &mut s) == 0 || s.is_null() {
            return None;
        }
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
        LocalFree(s as *mut _);
        Some(out)
    }
}

/// Yol için değişiklik bildirimi tutamacı (alt klasörler dahil).
fn notify(path: &str) -> Option<HANDLE> {
    let w = wide(path);
    // SAFETY: w NUL sonlu ve çağrı boyunca canlı.
    let h = unsafe {
        FindFirstChangeNotificationW(
            w.as_ptr(),
            1,
            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE,
        )
    };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(h)
}

/// Sabit/çıkarılabilir sürücüler için tutamaçlar.
fn open_handles() -> Vec<HANDLE> {
    let sid = user_sid();
    let mut out = Vec::new();
    // SAFETY: parametresiz sorgu.
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        // SAFETY: root NUL sonlu.
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
            continue;
        }
        let base = format!("{letter}:\\$Recycle.Bin");
        let h = notify(&base).or_else(|| sid.as_ref().and_then(|s| notify(&format!("{base}\\{s}"))));
        if let Some(h) = h {
            out.push(h);
        }
    }
    out
}

fn watch(on_change: impl Fn(bool)) {
    let last = AtomicBool::new(is_full());
    on_change(last.load(Ordering::Relaxed));
    let check = |last: &AtomicBool| {
        let now = is_full();
        if last.swap(now, Ordering::Relaxed) != now {
            on_change(now);
        }
    };

    let handles = open_handles();
    if handles.is_empty() {
        loop {
            std::thread::sleep(POLL);
            check(&last);
        }
    }

    let n = handles.len() as u32;
    let wait = |ms: u32| -> u32 {
        // SAFETY: handles geçerli tutamaçlar, n uzunluğuyla eşleşir.
        unsafe { WaitForMultipleObjects(n, handles.as_ptr(), 0, ms) }
    };
    let rearm = |idx: u32| {
        // SAFETY: idx dizi sınırları içinde; tutamaç geçerli.
        unsafe { FindNextChangeNotification(handles[idx as usize]) };
    };
    loop {
        let r = wait(SAFETY.as_millis() as u32);
        if r == WAIT_TIMEOUT {
            check(&last);
            continue;
        }
        let Some(idx) = r.checked_sub(WAIT_OBJECT_0).filter(|i| *i < n) else {
            // Beklenmeyen hata: kısa bekleyip yeniden dene.
            std::thread::sleep(Duration::from_secs(1));
            check(&last);
            continue;
        };
        rearm(idx);
        // Sessizlik olana kadar bekle.
        loop {
            let r = wait(DEBOUNCE_MS);
            match r.checked_sub(WAIT_OBJECT_0).filter(|i| *i < n) {
                Some(i) => rearm(i),
                None => break,
            }
        }
        check(&last);
    }
    // Döngü süreç boyunca sürer; tutamaçlar süreçle birlikte kapanır.
}
