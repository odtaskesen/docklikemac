//! Kendi mesaj döngüsü olan yardımcı iş parçacığı. Hepsi olay tabanlıdır:
//! - `Ctrl+Alt+D` genel kısayolu (RegisterHotKey)
//! - görev çubuğu kendiliğinden görünürse yeniden gizleme (SetWinEventHook)
//! - dock sağ tık menüsü (Win32 açılır menü, koyu tema)
//! - otomatik gizlemede ekranın alt kenarındaki görünmez tetikleyici şerit
//!
//! Menü kendi modal döngüsünü çalıştırdığı için Slint/winit döngüsünden ayrı
//! bir iş parçacığında tutulur. Sonuçlar `slint::invoke_from_event_loop` ile
//! ana iş parçacığına (`crate::dock`) iletilir.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DispatchMessageW, GetMessageW, LoadCursorW, PeekMessageW, PostMessageW, PostThreadMessageW,
    RegisterClassW, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowPos, ShowWindow,
    TrackPopupMenuEx, TranslateMessage, EVENT_OBJECT_SHOW, HMENU, HWND_TOPMOST, IDC_ARROW,
    LWA_ALPHA, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    OBJID_WINDOW, PM_NOREMOVE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINEVENT_OUTOFCONTEXT, WM_APP, WM_HOTKEY,
    WM_MOUSEMOVE, WM_NULL, WM_USER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::win::wide;
use crate::{appbar, dock, taskbar};

/// lparam = imleç (x, y) paketli; menü içeriği `PENDING_MENU` içinde.
const MSG_MENU: u32 = WM_APP + 10;
/// wparam = şeridin sol x'i (i32), lparam = genişlik.
pub const MSG_EDGE_SHOW: u32 = WM_APP + 11;
pub const MSG_EDGE_HIDE: u32 = WM_APP + 12;
/// Explorer yeniden başlayınca kancayı yeni sürece kur.
pub const MSG_REHOOK: u32 = WM_APP + 13;

/// Açılır menü öğesi.
pub enum MenuItem {
    Item { id: u32, text: String, checked: bool, enabled: bool },
    Separator,
    Sub { text: String, items: Vec<MenuItem> },
}

impl MenuItem {
    pub fn item(id: u32, text: impl Into<String>) -> Self {
        MenuItem::Item { id, text: text.into(), checked: false, enabled: true }
    }

    pub fn check(id: u32, text: impl Into<String>, checked: bool) -> Self {
        MenuItem::Item { id, text: text.into(), checked, enabled: true }
    }

    pub fn sub(text: impl Into<String>, items: Vec<MenuItem>) -> Self {
        MenuItem::Sub { text: text.into(), items }
    }
}

type MenuCallback = Box<dyn FnOnce(u32) + Send>;
static PENDING_MENU: Mutex<Option<(Vec<MenuItem>, MenuCallback)>> = Mutex::new(None);

/// Menüyü (x, y) ekran konumunda açar. Seçilen komut kimliği (iptalde 0)
/// UI iş parçacığında `on_result`'a verilir.
pub fn show_menu(items: Vec<MenuItem>, x: i32, y: i32, on_result: impl FnOnce(u32) + Send + 'static) {
    if let Ok(mut p) = PENDING_MENU.lock() {
        *p = Some((items, Box::new(on_result)));
    }
    post(MSG_MENU, 0, pack_point(x, y));
}

/// Tetikleyici şeridin yüksekliği (fiziksel piksel).
const EDGE_HEIGHT: i32 = 2;
const VK_D: u32 = 0x44;

static THREAD_ID: AtomicU32 = AtomicU32::new(0);
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
static EDGE: AtomicUsize = AtomicUsize::new(0);
static HOOK: AtomicUsize = AtomicUsize::new(0);

pub fn menu_open() -> bool {
    MENU_OPEN.load(Ordering::SeqCst)
}

pub fn post(msg: u32, wp: usize, lp: isize) {
    unsafe { PostThreadMessageW(THREAD_ID.load(Ordering::SeqCst), msg, wp, lp) };
}

fn pack_point(x: i32, y: i32) -> isize {
    (((y as u16 as u32) << 16) | (x as u16 as u32)) as isize
}

fn unpack_point(lp: isize) -> (i32, i32) {
    let v = lp as u32;
    ((v & 0xFFFF) as u16 as i16 as i32, (v >> 16) as u16 as i16 as i32)
}

/// İş parçacığını başlatır, hazır olana kadar bekler.
/// Dönüş: `Ctrl+Alt+D` kaydedildi mi.
pub fn spawn() -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || unsafe { run(tx) });
    rx.recv().unwrap_or(false)
}

fn defer(f: impl FnOnce() + Send + 'static) {
    let _ = slint::invoke_from_event_loop(f);
}

unsafe fn run(ready: mpsc::Sender<bool>) {
    let mut msg: MSG = std::mem::zeroed();
    // Mesaj kuyruğunu oluştur (PostThreadMessage bundan önce başarısız olur).
    PeekMessageW(&mut msg, std::ptr::null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
    THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);

    enable_dark_menus();

    let hinst = GetModuleHandleW(std::ptr::null());
    let cls = wide("DockShellHelper");
    let mut wc: WNDCLASSW = std::mem::zeroed();
    wc.lpfnWndProc = Some(helper_proc);
    wc.hInstance = hinst;
    wc.lpszClassName = cls.as_ptr();
    wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
    RegisterClassW(&wc);

    let empty = wide("");
    let owner = CreateWindowExW(
        WS_EX_TOOLWINDOW,
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
    let edge = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
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
    // Neredeyse tamamen saydam ama fare olaylarını alır.
    SetLayeredWindowAttributes(edge, 0, 1, LWA_ALPHA);
    EDGE.store(edge as usize, Ordering::SeqCst);

    let hotkey_ok =
        RegisterHotKey(std::ptr::null_mut(), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_D) != 0;
    install_hook();
    let _ = ready.send(hotkey_ok);

    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        if msg.hwnd.is_null() {
            match msg.message {
                WM_HOTKEY => defer(dock::toggle_auto_hide),
                MSG_MENU => run_menu(owner, msg.lParam),
                MSG_EDGE_SHOW => {
                    let m = appbar::primary_monitor_rect();
                    let x = msg.wParam as u32 as i32;
                    let w = msg.lParam as i32;
                    SetWindowPos(
                        edge,
                        HWND_TOPMOST,
                        x,
                        m.bottom - EDGE_HEIGHT,
                        w,
                        EDGE_HEIGHT,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                MSG_EDGE_HIDE => {
                    ShowWindow(edge, SW_HIDE);
                }
                MSG_REHOOK => {
                    let old = HOOK.swap(0, Ordering::SeqCst) as HWINEVENTHOOK;
                    if !old.is_null() {
                        UnhookWinEvent(old);
                    }
                    install_hook();
                }
                _ => {}
            }
            continue;
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe fn install_hook() {
    let hook = SetWinEventHook(
        EVENT_OBJECT_SHOW,
        EVENT_OBJECT_SHOW,
        std::ptr::null_mut(),
        Some(win_event),
        taskbar::explorer_pid(),
        0,
        WINEVENT_OUTOFCONTEXT,
    );
    if hook.is_null() {
        eprintln!("[kanca] SetWinEventHook başarısız");
    }
    HOOK.store(hook as usize, Ordering::SeqCst);
}

unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    h: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object == OBJID_WINDOW {
        taskbar::on_window_shown(h);
    }
}

unsafe extern "system" fn helper_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_MOUSEMOVE && h as usize == EDGE.load(Ordering::SeqCst) {
        ShowWindow(h, SW_HIDE);
        defer(dock::on_edge_hit);
        return 0;
    }
    DefWindowProcW(h, msg, wp, lp)
}

/// Menü öğelerinden Win32 menüsü kurar (alt menüler dahil).
unsafe fn build_menu(items: &[MenuItem]) -> HMENU {
    let menu = CreatePopupMenu();
    for it in items {
        match it {
            MenuItem::Item { id, text, checked, enabled } => {
                let mut flags = MF_STRING;
                if *checked {
                    flags |= MF_CHECKED;
                }
                if !*enabled {
                    flags |= MF_GRAYED;
                }
                AppendMenuW(menu, flags, *id as usize, wide(text).as_ptr());
            }
            MenuItem::Separator => {
                AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            }
            MenuItem::Sub { text, items } => {
                let sub = build_menu(items);
                AppendMenuW(menu, MF_POPUP, sub as usize, wide(text).as_ptr());
            }
        }
    }
    menu
}

unsafe fn run_menu(owner: HWND, lp: isize) {
    let Some((items, on_result)) = PENDING_MENU.lock().ok().and_then(|mut p| p.take()) else {
        return;
    };
    let (x, y) = unpack_point(lp);
    let menu = build_menu(&items);

    MENU_OPEN.store(true, Ordering::SeqCst);
    // Menünün dışına tıklanınca kapanabilmesi için sahibi ön plana alınmalı.
    SetForegroundWindow(owner);
    let cmd = TrackPopupMenuEx(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        x,
        y,
        owner,
        std::ptr::null(),
    );
    PostMessageW(owner, WM_NULL, 0, 0);
    DestroyMenu(menu); // alt menüyü de yok eder
    MENU_OPEN.store(false, Ordering::SeqCst);

    let cmd = cmd.max(0) as u32;
    defer(move || {
        on_result(cmd);
        dock::on_menu_closed();
    });
}

/// Win32 menülerini koyu temaya alır (uxtheme'in belgelenmemiş sıralı
/// fonksiyonları: 135 SetPreferredAppMode, 136 FlushMenuThemes).
unsafe fn enable_dark_menus() {
    let ux = LoadLibraryW(wide("uxtheme.dll").as_ptr());
    if ux.is_null() {
        return;
    }
    if let Some(f) = GetProcAddress(ux, 135usize as *const u8) {
        let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
        set_mode(2); // ForceDark
    }
    if let Some(f) = GetProcAddress(ux, 136usize as *const u8) {
        let flush: unsafe extern "system" fn() = std::mem::transmute(f);
        flush();
    }
}
