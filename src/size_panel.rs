//! Dock'un üstünde küçük bir kaydırıcı paneli (Win32 + DWM, GDI çizimi):
//! "Boyut…" (dock boyutu) ve ses (tekerlekle açılır, kendiliğinden kapanır).
//! Tek pencere; başka türde açılırsa öncekini kapatır.
//!
//! Sürükledikçe `on_change` canlı olarak çağrılır; panel kapanınca `on_close`
//! son değerle çağrılır (kaydetmek için). Geri çağrılar olay döngüsüne ertelenir
//! (pencere yordamı dock durumuna doğrudan dokunmaz).
//!
//! Panel odak almadığı için dışarı tıklamayı kendisi göremez: açıkken 50 ms'lik
//! bir zamanlayıcı fare düğmelerini ve Esc'i yoklar. Kapalıyken iş yapmaz.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, RoundRect,
    SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor, StretchBlt,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL,
    FW_SEMIBOLD, HALFTONE, HDC, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT, Arc, Polygon,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, KillTimer, SetTimer, SetWindowPos, MA_NOACTIVATE, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_NOZORDER, WM_CAPTURECHANGED,
    WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_TIMER,
};

use crate::{popup, win};

pub const MIN: f32 = 0.75;
pub const MAX: f32 = 1.25;

/// Panel türü.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Dock boyutu (0.75..1.25, %100'e mıknatıs).
    Size,
    /// Ses seviyesi (0..1). Dokunulmayınca kendiliğinden kapanır.
    Volume,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Kind::Size => "Dock boyutu",
            Kind::Volume => "Ses",
        }
    }
    fn range(self) -> (f32, f32) {
        match self {
            Kind::Size => (MIN, MAX),
            Kind::Volume => (0.0, 1.0),
        }
    }
    /// Tekerlek çentiği başına değişim.
    fn wheel_step(self) -> f32 {
        match self {
            Kind::Size => 0.05,
            Kind::Volume => 0.02,
        }
    }
    /// Panel genişliği (mantıksal).
    fn width(self) -> f32 {
        match self {
            Kind::Size => 300.0,
            Kind::Volume => 240.0,
        }
    }
}

/// Ses panelinin dokunulmadan açık kalma süresi.
const IDLE_CLOSE: Duration = Duration::from_millis(1500);

/// Panel ölçüleri (mantıksal piksel; ekran ölçeğiyle çarpılır).
const H: f32 = 64.0;
const PAD: f32 = 16.0;
/// Kaydırıcı çizgisinin dikey merkezi, sol ucu ve sağ uçtan uzaklığı.
const TRACK_Y: f32 = 44.0;
const TRACK_X0: f32 = 40.0;
const TRACK_R: f32 = 44.0;
const THUMB: f32 = 18.0;
/// Panelin alt kenarıyla dock penceresinin üstü arasındaki boşluk.
pub const GAP: f32 = 8.0;
/// Kenar yumuşatma için şekiller bu kat büyük çizilip küçültülür.
const SS: i32 = 4;

const BG: u32 = 0x242424;
const BORDER: u32 = 0x3c3c3c;
const TRACK: u32 = 0x4a4a4a;
const TICK: u32 = 0x6a6a6a;
const GLYPH: u32 = 0x8c8c8c;
const TEXT: u32 = 0xffffff;
const TEXT_DIM: u32 = 0xa8a8a8;

const TIMER_ID: usize = 1;

struct Panel {
    hwnd: HWND,
    kind: Kind,
    /// Son dokunuş (ses paneli bundan `IDLE_CLOSE` sonra kapanır).
    touched: Instant,
    open: bool,
    value: f32,
    dragging: bool,
    /// Ekran ölçeği (fiziksel / mantıksal).
    dpi: f32,
    accent: u32,
    on_change: fn(f32),
    on_close: fn(f32),
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|p| {
        let mut p = p.try_borrow_mut().ok()?;
        p.as_mut().map(f)
    })
}

fn defer(f: impl FnOnce() + Send + 'static) {
    let _ = slint::invoke_from_event_loop(f);
}

/// 0xRRGGBB → COLORREF (0x00BBGGRR).
fn cref(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

fn clamp_value(kind: Kind, v: f32) -> f32 {
    let v = (v * 100.0).round() / 100.0;
    // Boyut: %100'e yakınsa oraya otursun (varsayılan boyut kolay bulunsun).
    let v = if kind == Kind::Size && (v - 1.0).abs() < 0.025 { 1.0 } else { v };
    let (lo, hi) = kind.range();
    v.clamp(lo, hi)
}

/// Paneli açar. `center_x` ve `bottom` fiziksel ekran koordinatı: panel bu
/// noktanın üstünde, yatayda ortalı durur (ekrandan taşmaz). Aynı türde zaten
/// açıksa değeri ve yeri güncellenir; başka türde açıksa önce o kapanır.
#[allow(clippy::too_many_arguments)]
pub fn open(
    kind: Kind,
    value: f32,
    dpi: f32,
    accent: u32,
    center_x: i32,
    bottom: i32,
    on_change: fn(f32),
    on_close: fn(f32),
) {
    if with(|p| p.open && p.kind != kind) == Some(true) {
        close();
    }
    let hwnd = PANEL.with(|p| {
        let mut p = p.borrow_mut();
        let panel = p.get_or_insert_with(|| Panel {
            hwnd: popup::create("DockSizePanel", Some(wnd_proc), BORDER),
            kind,
            touched: Instant::now(),
            open: false,
            value,
            dragging: false,
            dpi,
            accent,
            on_change,
            on_close,
        });
        if !(panel.open && panel.kind == kind) {
            panel.dragging = false;
        }
        panel.open = true;
        panel.kind = kind;
        panel.touched = Instant::now();
        panel.value = clamp_value(kind, value);
        panel.dpi = dpi;
        panel.accent = accent;
        panel.on_change = on_change;
        panel.on_close = on_close;
        panel.hwnd
    });
    popup::show_at(hwnd, rect_for(kind, dpi, center_x, bottom));
    popup::redraw(hwnd);
    unsafe { SetTimer(hwnd, TIMER_ID, 50, None) };
}

/// Açık panelin türü.
pub fn kind() -> Option<Kind> {
    with(|p| p.open.then_some(p.kind)).flatten()
}

/// Değer dışarıda değişti (ör. ses başka yerden ayarlandı): `on_change` çağırmadan gösterir.
pub fn show_value(kind: Kind, v: f32) {
    let Some(hwnd) = with(|p| {
        if !p.open || p.kind != kind || p.dragging {
            return None;
        }
        let v = clamp_value(kind, v);
        if (p.value - v).abs() < f32::EPSILON {
            return None;
        }
        p.value = v;
        Some(p.hwnd)
    })
    .flatten() else {
        return;
    };
    popup::redraw(hwnd);
}

/// Ses paneli açık kalsın (dock üzerinde tekerlek döndükçe).
pub fn touch() {
    with(|p| p.touched = Instant::now());
}

/// Açıksa paneli verilen noktanın üstüne taşır (dock boyutu değişince).
pub fn move_to(center_x: i32, bottom: i32) {
    let Some((hwnd, dpi, kind)) = with(|p| p.open.then_some((p.hwnd, p.dpi, p.kind))).flatten() else {
        return;
    };
    let r = rect_for(kind, dpi, center_x, bottom);
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            r.left,
            r.top,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub fn is_open() -> bool {
    with(|p| p.open).unwrap_or(false)
}

/// Paneli kapatır, `on_close`'u son değerle çağırır.
pub fn close() {
    let Some((hwnd, cb, v)) = with(|p| {
        if !p.open {
            return None;
        }
        p.open = false;
        p.dragging = false;
        Some((p.hwnd, p.on_close, p.value))
    })
    .flatten() else {
        return;
    };
    unsafe {
        KillTimer(hwnd, TIMER_ID);
        ReleaseCapture();
    }
    popup::hide(hwnd);
    defer(move || cb(v));
}

fn rect_for(kind: Kind, dpi: f32, center_x: i32, bottom: i32) -> RECT {
    let w = (kind.width() * dpi).round() as i32;
    let h = (H * dpi).round() as i32;
    // Ekrandan taşmasın (ses paneli sağ kenara yakın açılır).
    let m = crate::appbar::primary_monitor_rect();
    let margin = (8.0 * dpi) as i32;
    let left = (center_x - w / 2).clamp(m.left + margin, (m.right - margin - w).max(m.left + margin));
    RECT { left, top: bottom - h, right: left + w, bottom }
}

/// Fare x'inden (istemci, fiziksel) değer.
fn value_at(p: &Panel, x: i32) -> f32 {
    let x0 = TRACK_X0 * p.dpi;
    let x1 = (p.kind.width() - TRACK_R) * p.dpi;
    let t = ((x as f32 - x0) / (x1 - x0)).clamp(0.0, 1.0);
    let (lo, hi) = p.kind.range();
    clamp_value(p.kind, lo + t * (hi - lo))
}

/// Değeri ayarlar; değiştiyse yeniden çizer ve `on_change`'i çağırır.
fn set_value(v: f32) {
    let Some((hwnd, cb, changed)) = with(|p| {
        let changed = (p.value - v).abs() > f32::EPSILON;
        p.value = v;
        p.touched = Instant::now();
        (p.hwnd, p.on_change, changed)
    }) else {
        return;
    };
    popup::redraw(hwnd);
    if changed {
        defer(move || cb(v));
    }
}

fn mouse_x(lp: LPARAM) -> i32 {
    (lp as u32 & 0xFFFF) as u16 as i16 as i32
}

unsafe extern "system" fn wnd_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            paint(h);
            return 0;
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            if let Some(v) = with(|p| {
                p.dragging = true;
                value_at(p, mouse_x(lp))
            }) {
                SetCapture(h);
                set_value(v);
            }
            return 0;
        }
        WM_MOUSEMOVE => {
            if let Some(Some(v)) = with(|p| p.dragging.then(|| value_at(p, mouse_x(lp)))) {
                set_value(v);
            }
            return 0;
        }
        WM_LBUTTONUP => {
            with(|p| p.dragging = false);
            ReleaseCapture();
            popup::redraw(h);
            return 0;
        }
        WM_CAPTURECHANGED => {
            if with(|p| std::mem::replace(&mut p.dragging, false)) == Some(true) {
                popup::redraw(h);
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16 as f32 / 120.0;
            if let Some(v) = with(|p| clamp_value(p.kind, p.value + delta * p.kind.wheel_step())) {
                set_value(v);
            }
            return 0;
        }
        WM_TIMER if wp == TIMER_ID => {
            poll_outside(h);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(h, msg, wp, lp)
}

/// Dışarıda bir fare düğmesine basıldıysa ya da Esc'e basıldıysa kapatır.
fn poll_outside(h: HWND) {
    let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
    if down(VK_ESCAPE) {
        close();
        return;
    }
    let Some((dragging, kind, touched)) = with(|p| (p.dragging, p.kind, p.touched)) else {
        return;
    };
    if dragging {
        return;
    }
    let inside = win::cursor_inside(h);
    if (down(VK_LBUTTON) || down(VK_RBUTTON) || down(VK_MBUTTON)) && !inside {
        close();
        return;
    }
    if kind == Kind::Volume {
        if inside {
            with(|p| p.touched = Instant::now());
        } else if touched.elapsed() >= IDLE_CLOSE {
            close();
        }
    }
}

// ---- Çizim ----

/// Seçili GDI nesnelerini kapsamdan çıkınca geri alıp siler.
struct Gdi {
    dc: HDC,
    old: *mut core::ffi::c_void,
    obj: *mut core::ffi::c_void,
}

impl Gdi {
    fn select(dc: HDC, obj: *mut core::ffi::c_void) -> Self {
        let old = unsafe { SelectObject(dc, obj) };
        Gdi { dc, old, obj }
    }
}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.obj);
        }
    }
}

fn paint(h: HWND) {
    let Some((kind, value, dpi, accent, dragging)) =
        with(|p| (p.kind, p.value, p.dpi, p.accent, p.dragging))
    else {
        return;
    };
    let (lo, hi) = kind.range();
    let pw = kind.width();
    let track_x1 = pw - TRACK_R;
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(h, &mut ps);
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(h, &mut rc);
        let (w, hh) = (rc.right, rc.bottom);

        // 1) Şekiller SS kat büyük bir bitmap'e.
        let big = CreateCompatibleDC(hdc);
        let big_bmp = Gdi::select(big, CreateCompatibleBitmap(hdc, w * SS, hh * SS));
        let s = dpi * SS as f32;
        let px = |v: f32| (v * s).round() as i32;
        let fill = |dc: HDC, color: u32, l: i32, t: i32, r: i32, b: i32, radius: i32| {
            let _b = Gdi::select(dc, CreateSolidBrush(cref(color)));
            let _p = Gdi::select(dc, CreatePen(PS_SOLID, 0, cref(color)));
            if radius > 0 {
                RoundRect(dc, l, t, r, b, radius * 2, radius * 2);
            } else {
                Ellipse(dc, l, t, r, b);
            }
        };
        let bg = CreateSolidBrush(cref(BG));
        FillRect(big, &RECT { left: 0, top: 0, right: w * SS, bottom: hh * SS }, bg);
        DeleteObject(bg);

        match kind {
            Kind::Size => {
                // Sol/sağ: küçük ve büyük dock simgesi (yuvarlak kare).
                let glyph = |cx: f32, size: f32| {
                    let r = size / 2.0;
                    fill(big, GLYPH, px(cx - r), px(TRACK_Y - r), px(cx + r), px(TRACK_Y + r), px(size * 0.25));
                };
                glyph(PAD + 6.0, 10.0);
                glyph(pw - PAD - 9.0, 18.0);
                // %100 işareti.
                let one = TRACK_X0 + (1.0 - lo) / (hi - lo) * (track_x1 - TRACK_X0);
                fill(big, TICK, px(one - 1.0), px(TRACK_Y - 6.0), px(one + 1.0), px(TRACK_Y + 6.0), px(1.0));
            }
            Kind::Volume => {
                // Sol: hoparlör; sağ: hoparlör + ses dalgaları.
                speaker(big, s, PAD - 2.0, false);
                speaker(big, s, pw - PAD - 16.0, true);
            }
        }

        // Çizgi, dolu kısım, tutamaç.
        let t = (value - lo) / (hi - lo);
        let tx = TRACK_X0 + t * (track_x1 - TRACK_X0);
        fill(big, TRACK, px(TRACK_X0), px(TRACK_Y - 2.0), px(track_x1), px(TRACK_Y + 2.0), px(2.0));
        fill(big, accent, px(TRACK_X0), px(TRACK_Y - 2.0), px(tx), px(TRACK_Y + 2.0), px(2.0));
        let r = THUMB / 2.0;
        fill(big, 0x1a1a1a, px(tx - r - 0.5), px(TRACK_Y - r + 0.5), px(tx + r + 0.5), px(TRACK_Y + r + 1.5), 0);
        let thumb = if dragging { 0xdcdcdc } else { 0xffffff };
        fill(big, thumb, px(tx - r), px(TRACK_Y - r), px(tx + r), px(TRACK_Y + r), 0);

        // 2) Ortalayarak küçült (kenar yumuşatma).
        let small = CreateCompatibleDC(hdc);
        let small_bmp = Gdi::select(small, CreateCompatibleBitmap(hdc, w, hh));
        SetStretchBltMode(small, HALFTONE);
        SetBrushOrgEx(small, 0, 0, std::ptr::null_mut());
        StretchBlt(small, 0, 0, w, hh, big, 0, 0, w * SS, hh * SS, SRCCOPY);
        drop(big_bmp);
        DeleteDC(big);

        // 3) Yazılar doğrudan son boyutta (keskin kalsın).
        SetBkMode(small, TRANSPARENT as i32);
        let font = |size: f32, weight: i32| {
            let face = win::wide("Segoe UI");
            CreateFontW(
                -((size * dpi).round() as i32),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                face.as_ptr(),
            )
        };
        let text_rect = RECT {
            left: (PAD * dpi) as i32,
            top: (8.0 * dpi) as i32,
            right: w - (PAD * dpi) as i32,
            bottom: (28.0 * dpi) as i32,
        };
        {
            let _f = Gdi::select(small, font(13.0, FW_SEMIBOLD as i32));
            SetTextColor(small, cref(TEXT));
            let mut r = text_rect;
            let s = win::wide(kind.title());
            DrawTextW(small, s.as_ptr(), -1, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }
        {
            let _f = Gdi::select(small, font(12.0, FW_NORMAL as i32));
            SetTextColor(small, cref(TEXT_DIM));
            let mut r = text_rect;
            let s = win::wide(&format!("%{}", (value * 100.0).round() as i32));
            DrawTextW(small, s.as_ptr(), -1, &mut r, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
        }

        BitBlt(hdc, 0, 0, w, hh, small, 0, 0, SRCCOPY);
        drop(small_bmp);
        DeleteDC(small);
        EndPaint(h, &ps);
    }
}

/// Hoparlör simgesi (büyük bitmap'e; `s` mantıksal → büyük piksel çarpanı, `x` sol kenar).
/// `waves`: sağında iki ses dalgası.
fn speaker(dc: HDC, s: f32, x: f32, waves: bool) {
    let px = |v: f32| (v * s).round() as i32;
    let y = TRACK_Y;
    let pt = |a: f32, b: f32| POINT { x: px(a), y: px(b) };
    let pts = [
        pt(x, y - 2.5),
        pt(x + 3.0, y - 2.5),
        pt(x + 7.0, y - 6.0),
        pt(x + 7.0, y + 6.0),
        pt(x + 3.0, y + 2.5),
        pt(x, y + 2.5),
    ];
    unsafe {
        let _b = Gdi::select(dc, CreateSolidBrush(cref(GLYPH)));
        let _p = Gdi::select(dc, CreatePen(PS_SOLID, 0, cref(GLYPH)));
        Polygon(dc, pts.as_ptr(), pts.len() as i32);
    }
    if waves {
        let _p = Gdi::select(dc, unsafe { CreatePen(PS_SOLID, px(1.4), cref(GLYPH)) });
        let cx = x + 7.0;
        for r in [4.0f32, 7.5] {
            // Sağa bakan yay (GDI saat yönünün tersine çizer: alttan üste).
            let (dx, dy) = (r * 0.6, r * 0.8);
            unsafe {
                Arc(dc, px(cx - r), px(y - r), px(cx + r), px(y + r), px(cx + dx), px(y + dy), px(cx + dx), px(y - dy));
            }
        }
    }
}
