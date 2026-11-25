//! Windows görev çubuğunu gizleme ve geri getirme.
//!
//! Yöntem: ana görev çubuğu "otomatik gizle" durumuna alınır (böylece ekranda
//! yer ayırmayı bırakır) ve tüm görev çubuğu pencereleri `SW_HIDE` ile gizlenir.
//! Asıl durum `%APPDATA%\Dock\taskbar_state.txt` dosyasına yazılır; dock çökerse
//! bir sonraki açılışta (veya `dock --restore` ile) bu dosyadan geri yüklenir.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetWindowThreadProcessId, ShowWindow, SW_HIDE, SW_SHOWNA,
};

use crate::win::wide;

/// Görev çubuğu şu an bizim tarafımızdan gizli mi?
static HIDDEN: AtomicBool = AtomicBool::new(false);
static ORIGINAL_STATE: AtomicU32 = AtomicU32::new(0);

fn state_file() -> PathBuf {
    crate::config::dir().join("taskbar_state.txt")
}

pub fn main_taskbar() -> HWND {
    unsafe { FindWindowW(wide("Shell_TrayWnd").as_ptr(), std::ptr::null()) }
}

/// Ana ve ikincil (diğer monitörlerdeki) görev çubuğu pencereleri.
pub fn taskbar_windows() -> Vec<HWND> {
    let mut v = Vec::new();
    let main = main_taskbar();
    if !main.is_null() {
        v.push(main);
    }
    let cls = wide("Shell_SecondaryTrayWnd");
    let mut h: HWND = std::ptr::null_mut();
    loop {
        h = unsafe { FindWindowExW(std::ptr::null_mut(), h, cls.as_ptr(), std::ptr::null()) };
        if h.is_null() {
            break;
        }
        v.push(h);
    }
    v
}

pub fn is_taskbar(h: HWND) -> bool {
    taskbar_windows().contains(&h)
}

/// Görev çubuğunun ait olduğu explorer sürecinin kimliği.
pub fn explorer_pid() -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(main_taskbar(), &mut pid) };
    pid
}

fn abd(h: HWND) -> APPBARDATA {
    let mut a: APPBARDATA = unsafe { std::mem::zeroed() };
    a.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
    a.hWnd = h;
    a
}

fn get_state(h: HWND) -> u32 {
    let mut a = abd(h);
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut a) as u32 }
}

fn set_state(h: HWND, state: u32) {
    let mut a = abd(h);
    a.lParam = state as LPARAM;
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut a) };
}

fn show_all(cmd: i32) {
    for h in taskbar_windows() {
        unsafe { ShowWindow(h, cmd) };
    }
}

/// Görev çubuğunu gizler ve yer ayırmasını kaldırır.
pub fn hide() {
    let tb = main_taskbar();
    if tb.is_null() {
        eprintln!("[görev çubuğu] Shell_TrayWnd bulunamadı");
        return;
    }
    // Önceki bir çökmeden kalan dosya varsa asıl durum odur.
    let file = state_file();
    let original = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or_else(|| get_state(tb));
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, original.to_string());
    ORIGINAL_STATE.store(original, Ordering::SeqCst);

    set_state(tb, ABS_AUTOHIDE);
    show_all(SW_HIDE);
    HIDDEN.store(true, Ordering::SeqCst);
}

/// Görev çubuğunu eski haline getirir. Birden fazla kez çağrılması güvenlidir.
pub fn restore() {
    if !HIDDEN.swap(false, Ordering::SeqCst) {
        return;
    }
    set_state(main_taskbar(), ORIGINAL_STATE.load(Ordering::SeqCst));
    show_all(SW_SHOWNA);
    let _ = std::fs::remove_file(state_file());
}

/// `dock --restore`: çökmeden sonra elle kurtarma.
pub fn restore_from_file() {
    let original = std::fs::read_to_string(state_file())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    set_state(main_taskbar(), original);
    show_all(SW_SHOWNA);
    let _ = std::fs::remove_file(state_file());
}

/// Explorer görev çubuğunu kendiliğinden gösterirse yeniden gizlemek için
/// `EVENT_OBJECT_SHOW` kancasından çağrılır.
pub fn on_window_shown(h: HWND) {
    if HIDDEN.load(Ordering::SeqCst) && is_taskbar(h) {
        eprintln!("[görev çubuğu] kendiliğinden göründü, yeniden gizleniyor");
        unsafe { ShowWindow(h, SW_HIDE) };
    }
}
