//! Dock'u bir "AppBar" olarak kaydeder: ekranın altında yer ayırır, böylece
//! maximize edilen pencereler dock'un üstünde durur.

use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows_sys::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows_sys::Win32::UI::WindowsAndMessaging::WM_APP;

pub const WM_APPBAR: u32 = WM_APP + 1;

static HWND_VAL: AtomicUsize = AtomicUsize::new(0);

fn abd(h: HWND) -> APPBARDATA {
    let mut a: APPBARDATA = unsafe { std::mem::zeroed() };
    a.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
    a.hWnd = h;
    a
}

/// Ana monitörün tam dikdörtgeni.
pub fn primary_monitor_rect() -> RECT {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        mi.rcMonitor
    }
}

pub fn register(h: HWND) {
    let mut a = abd(h);
    a.uCallbackMessage = WM_APPBAR;
    unsafe { SHAppBarMessage(ABM_NEW, &mut a) };
    HWND_VAL.store(h as usize, Ordering::SeqCst);
}

/// Alt kenarda `reserve` fiziksel piksel yükseklikte yer ayırır.
/// Ayrılan dikdörtgeni döner.
pub fn set_pos(reserve: i32) -> RECT {
    let h = HWND_VAL.load(Ordering::SeqCst) as HWND;
    let m = primary_monitor_rect();
    let mut a = abd(h);
    a.uEdge = ABE_BOTTOM;
    a.rc = RECT { left: m.left, right: m.right, top: m.bottom - reserve, bottom: m.bottom };
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut a);
        a.rc.top = a.rc.bottom - reserve;
        SHAppBarMessage(ABM_SETPOS, &mut a);
    }
    a.rc
}

/// Explorer yeniden başlayınca kayıt kaybolur; yeniden kaydet.
pub fn reregister(h: HWND) {
    let mut a = abd(h);
    unsafe { SHAppBarMessage(ABM_REMOVE, &mut a) };
    register(h);
}

/// Kaydı kaldırır. Birden fazla kez çağrılması güvenlidir.
pub fn unregister() {
    let h = HWND_VAL.swap(0, Ordering::SeqCst) as HWND;
    if h.is_null() {
        return;
    }
    let mut a = abd(h);
    unsafe { SHAppBarMessage(ABM_REMOVE, &mut a) };
}
