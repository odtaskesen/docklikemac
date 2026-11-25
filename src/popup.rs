//! Dock'un üstünde açılan küçük pencereler için ortak Win32 + DWM temeli (5a kararı).
//!
//! WS_POPUP, TOOLWINDOW + NOACTIVATE + TOPMOST: Alt+Tab'da görünmez, odak çalmaz.
//! DWM yuvarlak köşe ve kenarlık verir; aç/kapa geçiş animasyonu kapalıdır.
//! Çizim GDI ile pencerenin kendi `WM_PAINT`'inde yapılır. Pencereler UI iş
//! parçacığında oluşturulur; mesajlarını Slint'in olay döngüsü dağıtır.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{BOOL, HWND, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_TRANSITIONS_FORCEDISABLED,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::{InvalidateRect, UpdateWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, LoadCursorW, RegisterClassW, SetWindowPos, ShowWindow, HWND_TOPMOST,
    IDC_ARROW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WNDCLASSW, WNDPROC, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::win::wide;

/// Açılır pencere oluşturur (gizli). `class` her pencere türü için ayrı bir ad.
/// `border` 0xRRGGBB biçiminde DWM kenarlık rengi.
pub fn create(class: &str, proc_: WNDPROC, border: u32) -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let cls = wide(class);
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = proc_;
        wc.hInstance = hinst;
        wc.lpszClassName = cls.as_ptr();
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        RegisterClassW(&wc); // ikinci kez çağrılırsa zaten kayıtlı, sorun değil
        let empty = wide("");
        let h = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            cls.as_ptr(),
            empty.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        let round = DWMWCP_ROUND;
        set_attr(h, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &round);
        let off: BOOL = 1;
        set_attr(h, DWMWA_TRANSITIONS_FORCEDISABLED as u32, &off);
        let colorref = ((border & 0xFF) << 16) | (border & 0xFF00) | ((border >> 16) & 0xFF);
        set_attr(h, DWMWA_BORDER_COLOR as u32, &colorref);
        h
    }
}

fn set_attr<T>(h: HWND, attr: u32, v: &T) {
    unsafe {
        DwmSetWindowAttribute(h, attr, v as *const T as *const c_void, size_of::<T>() as u32);
    }
}

/// Pencereyi verilen yerde, odak çalmadan, en üstte gösterir ve hemen çizer.
pub fn show_at(h: HWND, r: RECT) {
    unsafe {
        SetWindowPos(
            h,
            HWND_TOPMOST,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        redraw(h);
    }
}

pub fn hide(h: HWND) {
    unsafe { ShowWindow(h, SW_HIDE) };
}

/// İçeriği hemen yeniden çizer (sürüklerken gecikmesin diye beklemeden).
pub fn redraw(h: HWND) {
    unsafe {
        InvalidateRect(h, std::ptr::null(), 0);
        UpdateWindow(h);
    }
}
