//! Küçük Win32 yardımcıları: pencere stilleri, cam efekti, bölge, tuş gönderme,
//! dock penceresinin alt sınıflanması.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateRectRgn, CreateRoundRectRgn, GetStockObject, SetWindowRgn, BLACK_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Ole::RevokeDragDrop;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_LWIN,
    VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP};

/// windows-sys bunu `Win32_UI_Controls` altında tutuyor; tek sabit için özellik açmayalım.
const WM_MOUSELEAVE: u32 = 0x02A3;
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, SetWindowSubclass, ABN_FULLSCREENAPP, ABN_POSCHANGED,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClassNameW, GetCursorPos, GetWindowLongPtrW,
    GetWindowRect, IsWindowVisible, RegisterClassW, RegisterWindowMessageW, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST, MA_NOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
    WINDOWPOS, WM_DISPLAYCHANGE, WM_DROPFILES, WM_ENDSESSION, WM_ERASEBKGND, WM_POWERBROADCAST, WM_MOUSEACTIVATE,
    WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WNDCLASSW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::{appbar, dock, widgets};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Alt+Tab'da görünmez, tıklanınca odak çalmaz, her zaman üstte.
pub fn make_tool_window(h: HWND) {
    unsafe {
        ShowWindow(h, SW_HIDE);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        let new = (ex | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST) & !WS_EX_APPWINDOW;
        SetWindowLongPtrW(h, GWL_EXSTYLE, new as isize);
        SetWindowPos(
            h,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        ShowWindow(h, SW_SHOWNOACTIVATE);
    }
}

pub fn window_rect(h: HWND) -> RECT {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(h, &mut r) };
    r
}

/// Pencerenin fiziksel boyutu (genişlik, yükseklik).
pub fn window_size(h: HWND) -> (i32, i32) {
    let r = window_rect(h);
    (r.right - r.left, r.bottom - r.top)
}

pub fn move_to(h: HWND, x: i32, y: i32) {
    unsafe { SetWindowPos(h, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE) };
}

pub fn show_no_activate(h: HWND) {
    unsafe {
        ShowWindow(h, SW_SHOWNOACTIVATE);
        SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

pub fn hide(h: HWND) {
    unsafe { ShowWindow(h, SW_HIDE) };
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

pub fn cursor_inside(h: HWND) -> bool {
    let (x, y) = cursor_pos();
    let r = window_rect(h);
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Pencerenin çizim ve tıklama alanını bir dikdörtgenle (isteğe bağlı yuvarlak
/// köşeli) sınırlar. `None` bölgeyi kaldırır (tüm pencere).
/// Sadece cam arka plan penceresinde kullanılır; dock penceresinin bölgesi
/// değiştirilmez (GPU ile çizilen pencerede tek karelik flaş yapıyordu).
pub fn set_region(h: HWND, rect: Option<(i32, i32, i32, i32)>, radius: i32) {
    unsafe {
        match rect {
            None => {
                SetWindowRgn(h, std::ptr::null_mut(), 0);
            }
            Some((x, y, w, hh)) => {
                let rgn = if radius > 0 {
                    CreateRoundRectRgn(x, y, x + w + 1, y + hh + 1, radius * 2, radius * 2)
                } else {
                    CreateRectRgn(x, y, x + w, y + hh)
                };
                SetWindowRgn(h, rgn, 0); // bölgenin sahibi artık sistem
            }
        }
    }
}

/// Cam modu için ayrı arka plan penceresi: sadece Acrylic bulanıklık, içerik yok.
/// Dock penceresinin hemen altında, gövdeyle aynı yerde durur.
pub fn create_backdrop() -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let cls = wide("DockBackdrop");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(DefWindowProcW);
        wc.hInstance = hinst;
        wc.lpszClassName = cls.as_ptr();
        // Siyah = saydam kabul edilir; arkasında bulanıklık görünür.
        wc.hbrBackground = GetStockObject(BLACK_BRUSH);
        RegisterClassW(&wc);
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
        set_acrylic(h, true);
        h
    }
}

/// Arka plan penceresini dock'un hemen altına yerleştirir ve gösterir.
pub fn place_below(backdrop: HWND, dock: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        SetWindowPos(backdrop, dock, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

pub fn is_visible(h: HWND) -> bool {
    unsafe { IsWindowVisible(h) != 0 }
}

/// Explorer'dan dosya bırakmayı WM_DROPFILES ile alır. winit'in OLE bırakma
/// hedefi bunu engellediği için önce kaldırılır.
pub fn accept_files(h: HWND) {
    unsafe {
        RevokeDragDrop(h);
        DragAcceptFiles(h, 1);
    }
}

/// Bırakılan dosyalar ve bırakma noktasının x'i (istemci, fiziksel px).
unsafe fn dropped_files(hdrop: HDROP) -> (Vec<String>, i32) {
    let mut pt = POINT { x: 0, y: 0 };
    DragQueryPoint(hdrop, &mut pt);
    let n = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
    let mut v = Vec::new();
    for i in 0..n {
        let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
        let mut buf = vec![0u16; len as usize + 1];
        DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
        v.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    DragFinish(hdrop);
    (v, pt.x)
}

pub fn shift_down() -> bool {
    unsafe { GetKeyState(VK_SHIFT as i32) < 0 }
}

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WinCompAttrData {
    attrib: u32,
    pv_data: *mut c_void,
    cb_data: usize,
}

type SetWca = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> BOOL;

/// Acrylic (bulanık cam) arka planı açar/kapatır. Blur'u Windows yapar.
pub fn set_acrylic(h: HWND, on: bool) {
    const WCA_ACCENT_POLICY: u32 = 19;
    const ACCENT_DISABLED: u32 = 0;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
    unsafe {
        let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
        let Some(f) = GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr() as _) else {
            eprintln!("[cam] SetWindowCompositionAttribute bulunamadı");
            return;
        };
        let f: SetWca = std::mem::transmute(f);
        let mut policy = AccentPolicy {
            accent_state: if on { ACCENT_ENABLE_ACRYLICBLURBEHIND } else { ACCENT_DISABLED },
            accent_flags: 2,
            // ABGR: hafif koyu tonlu cam
            gradient_color: 0x661C_1C1C,
            animation_id: 0,
        };
        let mut data = WinCompAttrData {
            attrib: WCA_ACCENT_POLICY,
            pv_data: &mut policy as *mut _ as *mut c_void,
            cb_data: std::mem::size_of::<AccentPolicy>(),
        };
        f(h, &mut data);
    }
}

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Tuş kombinasyonu gönderir: hepsine sırayla basar, ters sırayla bırakır.
pub fn send_combo(keys: &[u16]) {
    let mut v: Vec<INPUT> = keys.iter().map(|&k| key(k, false)).collect();
    v.extend(keys.iter().rev().map(|&k| key(k, true)));
    unsafe { SendInput(v.len() as u32, v.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
}

pub const KEY_WIN: u16 = VK_LWIN;
pub const KEY_ENTER: u16 = 0x0D;
pub const KEY_A: u16 = 0x41;
pub const KEY_B: u16 = 0x42;
pub const KEY_N: u16 = 0x4E;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Dock penceresinin mesajlarını yakalamak için alt sınıflama.
/// Buradan dock durumuna doğrudan dokunulmaz; olaylar olay döngüsüne ertelenir
/// (Win32 çağrıları eşzamanlı mesaj gönderdiği için yeniden girişi önler).
pub fn subclass(h: HWND) {
    unsafe {
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::SeqCst,
        );
        SetWindowSubclass(h, Some(subclass_proc), 1, 0);
    }
}

fn defer(f: impl FnOnce() + Send + 'static) {
    let _ = slint::invoke_from_event_loop(f);
}

unsafe extern "system" fn subclass_proc(
    h: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        // Bölge genişleyince Windows yeni alanı sınıf fırçasıyla (açık renk)
        // boyuyordu: tek karelik flaş. İçeriği zaten GPU çiziyor.
        WM_ERASEBKGND => return 1,
        appbar::WM_APPBAR => {
            match wp as u32 {
                ABN_POSCHANGED => defer(dock::on_display_changed),
                ABN_FULLSCREENAPP => {
                    let on = lp != 0;
                    defer(move || dock::on_fullscreen(on));
                }
                _ => {}
            }
            return 0;
        }
        WM_MOUSELEAVE => defer(dock::on_mouse_leave),
        // Slint pencere boyutunu sol üst köşe sabit kalacak şekilde değiştiriyor;
        // ardından yerleştirme ayrı bir karede yapılınca dock bir kare kayıyordu.
        // Görünürken boyut değişimini alt-ortaya sabitle (boyut + konum tek adımda).
        WM_WINDOWPOSCHANGING => {
            let pos = &mut *(lp as *mut WINDOWPOS);
            if pos.flags & SWP_NOSIZE == 0 && pos.flags & SWP_NOMOVE != 0 && is_visible(h) {
                let r = window_rect(h);
                let (w, hh) = (r.right - r.left, r.bottom - r.top);
                if (pos.cx, pos.cy) != (w, hh) {
                    pos.x = r.left + (w - pos.cx).div_euclid(2);
                    pos.y = r.bottom - pos.cy;
                    pos.flags &= !SWP_NOMOVE;
                }
            }
        }
        WM_WINDOWPOSCHANGED => {
            let pos = &*(lp as *const WINDOWPOS);
            if pos.flags & SWP_NOSIZE == 0 {
                defer(dock::on_resized);
            }
        }
        WM_DISPLAYCHANGE => defer(dock::on_display_changed),
        WM_POWERBROADCAST => defer(widgets::on_power),
        WM_DROPFILES => {
            let (files, x) = dropped_files(wp as HDROP);
            defer(move || widgets::on_files_dropped(files, x));
            return 0;
        }
        WM_ENDSESSION => crate::cleanup(),
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::SeqCst) => {
            defer(dock::on_explorer_restarted)
        }
        _ => {}
    }
    DefSubclassProc(h, msg, wp, lp)
}
