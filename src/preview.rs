//! Dock simgesinin üstünde pencere önizlemesi (Win32 + DWM, GDI çizimi):
//! uygulamanın açık pencereleri canlı DWM küçük resimleri ve başlıklarıyla
//! tek sırada. Tıklayınca o pencereye geçilir; üzerine gelince "×" çıkar.
//!
//! Geri çağrılar olay döngüsüne ertelenir (pencere yordamı dock durumuna
//! doğrudan dokunmaz). Panel odak almaz; dışarı tıklamayı 50 ms'lik bir
//! zamanlayıcı yoklar (`size_panel` ile aynı). Kapalıyken zamanlayıcı yoktur.
//!
//! Not: DWM küçük resimleri pencerenin tüm GDI içeriğinin üstüne çizilir. "×"
//! küçük resmin üstüne konamadığı için başlık satırının sağ ucundadır.

use std::cell::{Cell, RefCell};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY,
    DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, LineTo,
    MoveToEx, RoundRect, SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor,
    StretchBlt, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HALFTONE, HDC, PAINTSTRUCT, PS_SOLID,
    SRCCOPY, TRANSPARENT,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_LBUTTON,
    VK_MBUTTON, VK_RBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, KillTimer, SetTimer, MA_NOACTIVATE, WM_ERASEBKGND,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_PAINT, WM_TIMER,
};

use crate::{popup, win};

/// Önizlemedeki bir pencere.
pub struct Item {
    pub hwnd: isize,
    pub title: String,
    /// Pencere simge durumunda mı (küçültülmüş).
    pub minimized: bool,
}


// Ölçüler (mantıksal piksel).
const PAD: f32 = 10.0;
const ITEM_GAP: f32 = 10.0;
const TITLE_H: f32 = 20.0;
const TITLE_GAP: f32 = 4.0;
const BOX_H: f32 = 120.0;
const BOX_W_MIN: f32 = 90.0;
const BOX_W_MAX: f32 = 220.0;
const BOX_W_DEFAULT: f32 = 160.0;
const HILITE_PAD: f32 = 4.0;
const CLOSE_D: f32 = 18.0;
const BOX_RADIUS: f32 = 6.0;
/// Kenar yumuşatma için şekiller bu kat büyük çizilip küçültülür.
const SS: i32 = 3;

const BG: u32 = 0x242424;
const BORDER: u32 = 0x3c3c3c;
const BOX_BG: u32 = 0x2e2e2e;
const HILITE: u32 = 0x383838;
const CLOSE_BG: u32 = 0x505050;
const CLOSE_HOT: u32 = 0xc42b1c;
const TEXT: u32 = 0xffffff;
const TEXT_DIM: u32 = 0xa8a8a8;

const TIMER_ID: usize = 1;
/// WM_MOUSELEAVE (Win32_UI_Controls özelliği istememek için sabit).
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Bir öğenin yerleşimi (istemci alanı, fiziksel piksel).
#[derive(Clone, Copy, Default)]
struct Cell4 {
    /// Vurgu dikdörtgeni (başlık + kutu + dolgu).
    item: RECT4,
    title: RECT4,
    boxr: RECT4,
    close: RECT4,
}

#[derive(Clone, Copy, Default)]
struct RECT4 {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl RECT4 {
    fn has(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    fn rect(&self) -> RECT {
        RECT { left: self.l, top: self.t, right: self.r, bottom: self.b }
    }
}

/// Bir pencere için DWM küçük resim kaydı.
struct Thumb {
    src: isize,
    /// 0: kayıt başarısız (küçük resim yok).
    id: isize,
    sw: i32,
    sh: i32,
}

struct Panel {
    hwnd: HWND,
    open: bool,
    items: Vec<Item>,
    /// `items` ile aynı sırada.
    thumbs: Vec<Thumb>,
    lay: Vec<Cell4>,
    dpi: f32,
    accent: u32,
    hover: Option<usize>,
    hover_close: bool,
    /// Basılan öğe ve "×" üstünde mi basıldı.
    down: Option<(usize, bool)>,
    tracking: bool,
    on_pick: fn(isize),
    on_close_window: fn(isize),
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
    static ON_HIDDEN: Cell<Option<fn()>> = const { Cell::new(None) };
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

/// Panel kapandığında bir kez çağrılacak geri çağrı.
pub fn set_on_hidden(f: fn()) {
    ON_HIDDEN.with(|c| c.set(Some(f)));
}

pub fn is_open() -> bool {
    with(|p| p.open).unwrap_or(false)
}


/// İmleç önizleme penceresinin içinde mi.
pub fn cursor_inside() -> bool {
    match with(|p| p.open.then_some(p.hwnd)).flatten() {
        Some(h) => win::cursor_inside(h),
        None => false,
    }
}

/// Önizlemeyi açar ya da açıksa içeriğini ve yerini günceller (titremeden).
/// `center_x`, `bottom`: fiziksel ekran koordinatı; panel bu noktanın üstünde, yatayda ortalı.
/// `dpi`: fiziksel/mantıksal oran.
#[allow(clippy::too_many_arguments)]
pub fn show(
    items: Vec<Item>,
    center_x: i32,
    bottom: i32,
    dpi: f32,
    accent: u32,
    on_pick: fn(isize),
    on_close_window: fn(isize),
) {
    if items.is_empty() {
        hide();
        return;
    }
    let hwnd = PANEL.with(|p| {
        let mut p = p.borrow_mut();
        p.get_or_insert_with(|| Panel {
            hwnd: popup::create("DockPreview", Some(wnd_proc), BORDER),
            open: false,
            items: Vec::new(),
            thumbs: Vec::new(),
            lay: Vec::new(),
            dpi,
            accent,
            hover: None,
            hover_close: false,
            down: None,
            tracking: false,
            on_pick,
            on_close_window,
        })
        .hwnd
    });
    if hwnd.is_null() {
        return;
    }
    // Küçük resim kayıtlarını yeniden kullan; kalkanları sil, yenileri kaydet.
    let rect = with(|p| {
        let mut old = std::mem::take(&mut p.thumbs);
        let mut thumbs = Vec::with_capacity(items.len());
        for it in &items {
            let mut t = match old.iter().position(|t| t.src == it.hwnd) {
                Some(i) => old.swap_remove(i),
                None => register(p.hwnd, it.hwnd),
            };
            if t.id != 0 {
                let (sw, sh) = source_size(t.id);
                t.sw = sw;
                t.sh = sh;
            }
            thumbs.push(t);
        }
        for t in old {
            unregister(t.id);
        }
        let widths: Vec<f32> = thumbs.iter().map(|t| box_width(t.sw, t.sh)).collect();
        let (lay, w, h) = layout(&widths, dpi);
        p.thumbs = thumbs;
        p.lay = lay;
        p.items = items;
        p.dpi = dpi;
        p.accent = accent;
        p.on_pick = on_pick;
        p.on_close_window = on_close_window;
        p.hover = None;
        p.hover_close = false;
        p.down = None;
        p.open = true;
        rect_for(w, h, dpi, center_x, bottom)
    });
    let Some(rect) = rect else { return };
    popup::show_at(hwnd, rect);
    with(apply_thumbs);
    popup::redraw(hwnd);
    unsafe { SetTimer(hwnd, TIMER_ID, 50, None) };
}

/// Önizlemeyi kapatır; her şeyi serbest bırakır.
pub fn hide() {
    let Some((hwnd, thumbs)) = with(|p| {
        if !p.open {
            return None;
        }
        p.open = false;
        p.items.clear();
        p.lay.clear();
        p.hover = None;
        p.down = None;
        Some((p.hwnd, std::mem::take(&mut p.thumbs)))
    })
    .flatten() else {
        return;
    };
    for t in thumbs {
        unregister(t.id);
    }
    unsafe { KillTimer(hwnd, TIMER_ID) };
    popup::hide(hwnd);
    if let Some(f) = ON_HIDDEN.with(|c| c.get()) {
        defer(f);
    }
}

// ---- DWM küçük resimleri ----

fn register(dest: HWND, src: isize) -> Thumb {
    let mut id: isize = 0;
    let hr = unsafe { DwmRegisterThumbnail(dest, src as HWND, &mut id) };
    if hr < 0 {
        id = 0;
    }
    Thumb { src, id, sw: 0, sh: 0 }
}

fn unregister(id: isize) {
    if id != 0 {
        unsafe { DwmUnregisterThumbnail(id) };
    }
}

fn source_size(id: isize) -> (i32, i32) {
    let mut sz = SIZE { cx: 0, cy: 0 };
    let hr = unsafe { DwmQueryThumbnailSourceSize(id, &mut sz) };
    if hr < 0 {
        (0, 0)
    } else {
        (sz.cx, sz.cy)
    }
}

/// Kutu genişliği (mantıksal): kaynak en-boy oranından.
fn box_width(sw: i32, sh: i32) -> f32 {
    if sw <= 0 || sh <= 0 {
        return BOX_W_DEFAULT;
    }
    (BOX_H * sw as f32 / sh as f32).clamp(BOX_W_MIN, BOX_W_MAX)
}

/// Küçük resimlerin hedef dikdörtgenlerini (kutuya sığacak biçimde ortalı) günceller.
fn apply_thumbs(p: &mut Panel) {
    for (t, c) in p.thumbs.iter().zip(&p.lay) {
        if t.id == 0 {
            continue;
        }
        let b = c.boxr;
        let (bw, bh) = (b.r - b.l, b.b - b.t);
        let (w, h) = if t.sw > 0 && t.sh > 0 {
            let k = (bw as f32 / t.sw as f32).min(bh as f32 / t.sh as f32);
            (((t.sw as f32 * k).round() as i32).max(1), ((t.sh as f32 * k).round() as i32).max(1))
        } else {
            (bw, bh)
        };
        let l = b.l + (bw - w) / 2;
        let tp = b.t + (bh - h) / 2;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_VISIBLE
                | DWM_TNP_SOURCECLIENTAREAONLY
                | DWM_TNP_OPACITY,
            rcDestination: RECT { left: l, top: tp, right: l + w, bottom: tp + h },
            rcSource: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            opacity: 255,
            fVisible: 1,
            fSourceClientAreaOnly: 0,
        };
        unsafe { DwmUpdateThumbnailProperties(t.id, &props) };
    }
}

// ---- Yerleşim ----

/// Kutu genişliklerinden (mantıksal) yerleşim, panel genişliği ve yüksekliği (fiziksel).
fn layout(widths: &[f32], dpi: f32) -> (Vec<Cell4>, i32, i32) {
    let n = widths.len() as f32;
    let total = (PAD * 2.0 + widths.iter().sum::<f32>() + ITEM_GAP * (n - 1.0).max(0.0)) * dpi;
    let m = crate::appbar::primary_monitor_rect();
    let max = ((m.right - m.left) as f32 - 32.0 * dpi).max(1.0);
    let f = if total > max { (max / total).clamp(0.5, 1.0) } else { 1.0 };
    let s = dpi * f;
    let px = |v: f32| (v * s).round() as i32;

    let mut lay = Vec::with_capacity(widths.len());
    let mut x = px(PAD);
    for &w in widths {
        let bw = px(w);
        let top = px(PAD);
        let box_top = top + px(TITLE_H) + px(TITLE_GAP);
        let box_b = box_top + px(BOX_H);
        let hp = px(HILITE_PAD);
        let d = px(CLOSE_D);
        let cy = top + (px(TITLE_H) - d) / 2;
        lay.push(Cell4 {
            item: RECT4 { l: x - hp, t: top - hp, r: x + bw + hp, b: box_b + hp },
            title: RECT4 { l: x, t: top, r: x + bw - d - px(4.0), b: top + px(TITLE_H) },
            boxr: RECT4 { l: x, t: box_top, r: x + bw, b: box_b },
            close: RECT4 { l: x + bw - d, t: cy, r: x + bw, b: cy + d },
        });
        x += bw + px(ITEM_GAP);
    }
    let w = x - px(ITEM_GAP) + px(PAD);
    let h = px(PAD) * 2 + px(TITLE_H) + px(TITLE_GAP) + px(BOX_H);
    (lay, w, h)
}

fn rect_for(w: i32, h: i32, dpi: f32, center_x: i32, bottom: i32) -> RECT {
    let m = crate::appbar::primary_monitor_rect();
    let margin = (16.0 * dpi) as i32;
    let left = (center_x - w / 2).clamp(m.left + margin, (m.right - margin - w).max(m.left + margin));
    RECT { left, top: bottom - h, right: left + w, bottom }
}

// ---- Fare ----

fn mouse_pos(lp: LPARAM) -> (i32, i32) {
    (
        (lp as u32 & 0xFFFF) as u16 as i16 as i32,
        ((lp as u32 >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

/// Noktadaki öğe ve "×" üstünde mi.
fn hit(p: &Panel, x: i32, y: i32) -> Option<(usize, bool)> {
    let i = p.lay.iter().position(|c| c.item.has(x, y))?;
    Some((i, p.lay[i].close.has(x, y)))
}

unsafe extern "system" fn wnd_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            paint(h);
            return 0;
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lp);
            let (changed, track) = with(|p| {
                let (hv, hc) = match hit(p, x, y) {
                    Some((i, c)) => (Some(i), c),
                    None => (None, false),
                };
                let changed = hv != p.hover || hc != p.hover_close;
                p.hover = hv;
                p.hover_close = hc;
                let track = !p.tracking;
                p.tracking = true;
                (changed, track)
            })
            .unwrap_or((false, false));
            if track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: h,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
            }
            if changed {
                popup::redraw(h);
            }
            return 0;
        }
        WM_MOUSELEAVE => {
            let changed = with(|p| {
                p.tracking = false;
                let c = p.hover.is_some();
                p.hover = None;
                p.hover_close = false;
                c
            })
            .unwrap_or(false);
            if changed {
                popup::redraw(h);
            }
            return 0;
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = mouse_pos(lp);
            with(|p| p.down = hit(p, x, y));
            return 0;
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_pos(lp);
            let act = with(|p| {
                let down = p.down.take()?;
                let up = hit(p, x, y)?;
                (down == up).then(|| (p.items[up.0].hwnd, up.1, p.on_pick, p.on_close_window))
            })
            .flatten();
            if let Some((target, on_x, pick, close_win)) = act {
                if on_x {
                    defer(move || close_win(target));
                } else {
                    defer(move || pick(target));
                    hide();
                }
            }
            return 0;
        }
        WM_MBUTTONUP => {
            let (x, y) = mouse_pos(lp);
            let act = with(|p| {
                let (i, _) = hit(p, x, y)?;
                Some((p.items[i].hwnd, p.on_close_window))
            })
            .flatten();
            if let Some((target, close_win)) = act {
                defer(move || close_win(target));
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
        hide();
        return;
    }
    if (down(VK_LBUTTON) || down(VK_RBUTTON) || down(VK_MBUTTON)) && !win::cursor_inside(h) {
        hide();
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

type Snapshot = (Vec<(String, bool)>, Vec<Cell4>, f32, u32, Option<usize>, bool);

fn paint(h: HWND) {
    let Some((items, lay, dpi, accent, hover, hover_close)): Option<Snapshot> = with(|p| {
        (
            p.items.iter().map(|i| (i.title.clone(), i.minimized)).collect(),
            p.lay.clone(),
            p.dpi,
            p.accent,
            p.hover,
            p.hover_close,
        )
    }) else {
        return;
    };
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(h, &mut ps);
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(h, &mut rc);
        let (w, hh) = (rc.right, rc.bottom);

        // Yazı boyutu yerleşimle aynı ölçekte olsun (küçültme varsa kutu yüksekliğinden).
        let s_eff = lay
            .first()
            .map(|c| (c.boxr.b - c.boxr.t) as f32 / BOX_H)
            .filter(|v| *v > 0.0)
            .unwrap_or(dpi);

        // 1) Şekiller SS kat büyük bir bitmap'e.
        let big = CreateCompatibleDC(hdc);
        let big_bmp = Gdi::select(big, CreateCompatibleBitmap(hdc, w * SS, hh * SS));
        let ss = SS as f32;
        let b = |v: i32| v * SS;
        let fill = |dc: HDC, color: u32, r: RECT4, radius: i32| {
            let _b = Gdi::select(dc, CreateSolidBrush(cref(color)));
            let _p = Gdi::select(dc, CreatePen(PS_SOLID, 0, cref(color)));
            if radius > 0 {
                RoundRect(dc, b(r.l), b(r.t), b(r.r), b(r.b), radius * 2 * SS, radius * 2 * SS);
            } else {
                Ellipse(dc, b(r.l), b(r.t), b(r.r), b(r.b));
            }
        };
        let bg = CreateSolidBrush(cref(BG));
        FillRect(big, &RECT { left: 0, top: 0, right: w * SS, bottom: hh * SS }, bg);
        DeleteObject(bg);

        let rad = (BOX_RADIUS * s_eff).round() as i32;
        let hp = (2.0 * s_eff).round().max(1.0) as i32;
        for (i, c) in lay.iter().enumerate() {
            if hover == Some(i) {
                let inset = |r: RECT4, d: i32| RECT4 { l: r.l + d, t: r.t + d, r: r.r - d, b: r.b - d };
                let one = (s_eff.round() as i32).max(1);
                fill(big, accent, c.item, rad + hp);
                fill(big, HILITE, inset(c.item, one), rad + hp - one);
            }
            fill(big, BOX_BG, c.boxr, rad);
            if hover == Some(i) {
                let hot = hover_close;
                fill(big, if hot { CLOSE_HOT } else { CLOSE_BG }, c.close, 0);
                // "×": iki çizgi.
                let d = (c.close.r - c.close.l) as f32;
                let cx = (c.close.l + c.close.r) as f32 / 2.0;
                let cy = (c.close.t + c.close.b) as f32 / 2.0;
                let k = d * 0.2;
                let _p = Gdi::select(
                    big,
                    CreatePen(PS_SOLID, (1.5 * s_eff * ss).round() as i32, cref(TEXT)),
                );
                let pt = |v: f32| (v * ss).round() as i32;
                MoveToEx(big, pt(cx - k), pt(cy - k), std::ptr::null_mut());
                LineTo(big, pt(cx + k), pt(cy + k));
                MoveToEx(big, pt(cx + k), pt(cy - k), std::ptr::null_mut());
                LineTo(big, pt(cx - k), pt(cy + k));
            }
        }

        // 2) Küçült (kenar yumuşatma).
        let small = CreateCompatibleDC(hdc);
        let small_bmp = Gdi::select(small, CreateCompatibleBitmap(hdc, w, hh));
        SetStretchBltMode(small, HALFTONE);
        SetBrushOrgEx(small, 0, 0, std::ptr::null_mut());
        StretchBlt(small, 0, 0, w, hh, big, 0, 0, w * SS, hh * SS, SRCCOPY);
        drop(big_bmp);
        DeleteDC(big);

        // 3) Yazılar doğrudan son boyutta.
        SetBkMode(small, TRANSPARENT as i32);
        let face = win::wide("Segoe UI");
        let make_font = |size: f32| {
            CreateFontW(
                -((size * s_eff).round() as i32),
                0,
                0,
                0,
                FW_NORMAL as i32,
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
        {
            let _f = Gdi::select(small, make_font(12.0));
            for (c, (title, minimized)) in lay.iter().zip(&items) {
                let t = win::wide(title);
                SetTextColor(small, cref(TEXT));
                let mut r = c.title.rect();
                DrawTextW(
                    small,
                    t.as_ptr(),
                    -1,
                    &mut r,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
                if *minimized {
                    // Küçük resim boş gelebilir: kutuda soluk başlık (küçük resim üstüne çizilir).
                    SetTextColor(small, cref(TEXT_DIM));
                    let mut r = c.boxr.rect();
                    let pad = (8.0 * s_eff) as i32;
                    r.left += pad;
                    r.right -= pad;
                    DrawTextW(
                        small,
                        t.as_ptr(),
                        -1,
                        &mut r,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                    );
                }
            }
        }

        BitBlt(hdc, 0, 0, w, hh, small, 0, 0, SRCCOPY);
        drop(small_bmp);
        DeleteDC(small);
        EndPaint(h, &ps);
    }
}
