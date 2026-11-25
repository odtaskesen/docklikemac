//! Yığın ızgarası (5f): dock'taki bir klasörün içeriği, en yeni önce, ızgarada.
//!
//! Win32 + DWM açılır pencere (`popup`), GDI çizimi (`preview` ile aynı düzen:
//! şekiller büyük çizilip küçültülür, yazılar ve ikonlar son boyutta). İkonlar
//! `stacks::load_icons` iş parçacığından gelir ve geldikçe çizilir. Dörtten çok
//! satır varsa tekerlekle kayar. Tık: dosyayı açar; sağ tık: Aç / Klasörde Göster;
//! başlıktaki "Gezgin'de Aç" klasörü açar. Dışarı tıklayınca ya da Esc ile kapanır
//! (50 ms yoklama, sadece açıkken). Kapanınca ikonlar serbest bırakılır.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AlphaBlend, BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint,
    FillRect, IntersectClipRect, InvalidateRect, RoundRect, ScreenToClient, SelectObject,
    SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor, StretchBlt, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLEARTYPE_QUALITY,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_EDITCONTROL, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FW_NORMAL, FW_SEMIBOLD,
    HALFTONE, HBITMAP, HDC, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_LBUTTON,
    VK_MBUTTON, VK_RBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, GetCursorPos, KillTimer, SetTimer, MA_NOACTIVATE,
    WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_TIMER,
};

use crate::shell_thread::{self, MenuItem};
use crate::{popup, stacks, win};

/// Izgaradaki bir öğe.
pub struct Item {
    pub name: String,
    pub path: String,
}

// Ölçüler (mantıksal piksel).
const PAD: f32 = 12.0;
const HEADER_H: f32 = 28.0;
const HEADER_GAP: f32 = 4.0;
const CELL_W: f32 = 92.0;
const CELL_H: f32 = 104.0;
const ICON: f32 = 56.0;
const ICON_TOP: f32 = 8.0;
const TEXT_GAP: f32 = 6.0;
const TEXT_SIDE: f32 = 4.0;
const REVEAL_W: f32 = 130.0;
const RADIUS: f32 = 8.0;
const BAR_W: f32 = 4.0;
const ROWS_MAX: usize = 4;
const COLS_MAX: usize = 7;
/// Kenar yumuşatma için şekiller bu kat büyük çizilip küçültülür.
const SS: i32 = 3;

const BG: u32 = 0x242424;
const BORDER: u32 = 0x3c3c3c;
const HILITE: u32 = 0x3a3a3a;
const PRESSED: u32 = 0x474747;
const BAR: u32 = 0x5a5a5a;
const TEXT: u32 = 0xffffff;
const TEXT_DIM: u32 = 0xa8a8a8;

const TIMER_ID: usize = 1;
/// WM_MOUSELEAVE (Win32_UI_Controls özelliği istememek için sabit).
const WM_MOUSELEAVE: u32 = 0x02A3;

const CMD_OPEN: u32 = 1;
const CMD_REVEAL: u32 = 2;

/// Fare altındaki yer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spot {
    Item(usize),
    Reveal,
}

/// Bir ikonun GDI bitmap'i (premultiplied, AlphaBlend için).
struct Dib {
    bmp: HBITMAP,
    w: i32,
    h: i32,
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe { DeleteObject(self.bmp) };
    }
}

/// Fiziksel piksel yerleşimi.
#[derive(Clone, Copy, Default)]
struct Geo {
    w: i32,
    h: i32,
    pad: i32,
    cell_w: i32,
    cell_h: i32,
    grid_top: i32,
    grid_bottom: i32,
    cols: usize,
    /// Kaydırma payı (0: kaymaz).
    max_scroll: i32,
}

struct Grid {
    hwnd: HWND,
    open: bool,
    dir: String,
    title: String,
    items: Vec<Item>,
    icons: Vec<Option<Dib>>,
    dpi: f32,
    accent: u32,
    geo: Geo,
    scroll: i32,
    hover: Option<Spot>,
    down: Option<Spot>,
    tracking: bool,
    /// Her açılışta artar; eski iş parçacığının ikonları atılır.
    gen: u64,
    cancel: Option<Arc<AtomicBool>>,
    /// En son ne zaman ve hangi klasör için kapandı (dock'taki ikona tıklayınca
    /// dışarı tıklama yoklaması kapatır; aynı tık yeniden açmasın diye).
    closed: Option<(String, Instant)>,
}

thread_local! {
    static GRID: RefCell<Option<Grid>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Grid) -> R) -> Option<R> {
    GRID.with(|g| {
        let mut g = g.try_borrow_mut().ok()?;
        g.as_mut().map(f)
    })
}

fn defer(f: impl FnOnce() + Send + 'static) {
    let _ = slint::invoke_from_event_loop(f);
}

/// 0xRRGGBB → COLORREF (0x00BBGGRR).
fn cref(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

pub fn is_open() -> bool {
    with(|g| g.open).unwrap_or(false)
}

/// Bu klasörün ızgarası açık mı.
pub fn is_open_for(dir: &str) -> bool {
    with(|g| g.open && g.dir.eq_ignore_ascii_case(dir)).unwrap_or(false)
}

/// Bu klasörün ızgarası az önce (dock'a basılınca) kapandı mı.
pub fn just_closed(dir: &str) -> bool {
    with(|g| {
        g.closed
            .as_ref()
            .is_some_and(|(d, t)| d.eq_ignore_ascii_case(dir) && t.elapsed() < Duration::from_millis(400))
    })
    .unwrap_or(false)
}

/// Izgarayı açar. `center_x`, `bottom`: fiziksel ekran koordinatı; pencere bu
/// noktanın üstünde, yatayda ortalı. `dpi`: fiziksel/mantıksal oran.
pub fn show(dir: String, title: String, items: Vec<Item>, center_x: i32, bottom: i32, dpi: f32, accent: u32) {
    hide();
    let hwnd = GRID.with(|g| {
        let mut g = g.borrow_mut();
        g.get_or_insert_with(|| Grid {
            hwnd: popup::create("DockStack", Some(wnd_proc), BORDER),
            open: false,
            dir: String::new(),
            title: String::new(),
            items: Vec::new(),
            icons: Vec::new(),
            dpi,
            accent,
            geo: Geo::default(),
            scroll: 0,
            hover: None,
            down: None,
            tracking: false,
            gen: 0,
            cancel: None,
            closed: None,
        })
        .hwnd
    });
    if hwnd.is_null() {
        return;
    }
    let paths: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
    let Some((rect, gen, size)) = with(|g| {
        let m = crate::appbar::primary_monitor_rect();
        let max_h = bottom - m.top - (16.0 * dpi) as i32;
        g.geo = layout(items.len(), dpi, max_h);
        g.icons = items.iter().map(|_| None).collect();
        g.items = items;
        g.dir = dir;
        g.title = title;
        g.dpi = dpi;
        g.accent = accent;
        g.scroll = 0;
        g.hover = None;
        g.down = None;
        g.open = true;
        g.closed = None;
        g.gen += 1;
        let margin = (16.0 * dpi) as i32;
        let (w, h) = (g.geo.w, g.geo.h);
        let left = (center_x - w / 2).clamp(m.left + margin, (m.right - margin - w).max(m.left + margin));
        let rect = RECT { left, top: bottom - h, right: left + w, bottom };
        (rect, g.gen, (ICON * dpi).round() as i32)
    }) else {
        return;
    };
    popup::show_at(hwnd, rect);
    unsafe { SetTimer(hwnd, TIMER_ID, 50, None) };
    if !paths.is_empty() {
        let cancel = stacks::load_icons(paths, size, move |i, bmp| {
            defer(move || set_icon(gen, i, bmp));
        });
        with(|g| g.cancel = Some(cancel));
    }
}

/// Izgarayı kapatır; ikonları serbest bırakır.
pub fn hide() {
    let Some(hwnd) = with(|g| {
        if !g.open {
            return None;
        }
        g.open = false;
        g.closed = Some((std::mem::take(&mut g.dir), Instant::now()));
        if let Some(c) = g.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        g.items.clear();
        g.icons.clear();
        g.hover = None;
        g.down = None;
        Some(g.hwnd)
    })
    .flatten() else {
        return;
    };
    unsafe { KillTimer(hwnd, TIMER_ID) };
    popup::hide(hwnd);
    defer(|| {
        crate::dock::on_menu_closed();
        crate::widgets::on_stack_closed();
    });
}

fn set_icon(gen: u64, i: usize, bmp: stacks::Bitmap) {
    let hwnd = with(|g| {
        if !g.open || g.gen != gen || i >= g.icons.len() {
            return None;
        }
        g.icons[i] = make_dib(&bmp);
        Some(g.hwnd)
    })
    .flatten();
    if let Some(h) = hwnd {
        // UpdateWindow yok: çok sayıda ikon art arda gelince WM_PAINT birleşir.
        unsafe { InvalidateRect(h, std::ptr::null(), 0) };
    }
}

fn make_dib(b: &stacks::Bitmap) -> Option<Dib> {
    if b.w <= 0 || b.h <= 0 || b.bgra.len() != (b.w * b.h * 4) as usize {
        return None;
    }
    unsafe {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: b.w,
            biHeight: -b.h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(std::ptr::null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if bmp.is_null() || bits.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(b.bgra.as_ptr(), bits as *mut u8, b.bgra.len());
        Some(Dib { bmp, w: b.w, h: b.h })
    }
}

// ---- Yerleşim ----

/// Sütun sayısı: öğe sayısına göre yaklaşık kare, 3..7 (azsa öğe sayısı kadar).
fn columns(n: usize) -> usize {
    if n <= 3 {
        return n.max(3);
    }
    ((n as f32).sqrt().ceil() as usize).clamp(3, COLS_MAX)
}

fn layout(n: usize, dpi: f32, max_h: i32) -> Geo {
    let px = |v: f32| (v * dpi).round() as i32;
    let cols = columns(n);
    let rows = n.div_ceil(cols).max(1);
    let pad = px(PAD);
    let cell_w = px(CELL_W);
    let cell_h = px(CELL_H);
    let grid_top = pad + px(HEADER_H) + px(HEADER_GAP);
    // Ekrana sığacak kadar satır.
    let fit = ((max_h - grid_top - pad) / cell_h.max(1)).max(1) as usize;
    let vis = rows.min(ROWS_MAX).min(fit);
    let grid_h = if n == 0 { px(CELL_H * 0.6) } else { vis as i32 * cell_h };
    Geo {
        w: pad * 2 + cols as i32 * cell_w,
        h: grid_top + grid_h + pad,
        pad,
        cell_w,
        cell_h,
        grid_top,
        grid_bottom: grid_top + grid_h,
        cols,
        max_scroll: ((rows - vis) as i32 * cell_h).max(0),
    }
}

/// i. öğenin hücresi (istemci alanı, kaydırma uygulanmış).
fn cell_rect(geo: &Geo, scroll: i32, i: usize) -> RECT {
    let (col, row) = ((i % geo.cols) as i32, (i / geo.cols) as i32);
    let l = geo.pad + col * geo.cell_w;
    let t = geo.grid_top + row * geo.cell_h - scroll;
    RECT { left: l, top: t, right: l + geo.cell_w, bottom: t + geo.cell_h }
}

fn reveal_rect(geo: &Geo, dpi: f32) -> RECT {
    RECT {
        left: geo.w - geo.pad - (REVEAL_W * dpi).round() as i32,
        top: geo.pad,
        right: geo.w - geo.pad,
        bottom: geo.pad + (HEADER_H * dpi).round() as i32,
    }
}

fn has(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn hit(g: &Grid, x: i32, y: i32) -> Option<Spot> {
    if has(&reveal_rect(&g.geo, g.dpi), x, y) {
        return Some(Spot::Reveal);
    }
    if y < g.geo.grid_top || y >= g.geo.grid_bottom || x < g.geo.pad {
        return None;
    }
    let col = ((x - g.geo.pad) / g.geo.cell_w.max(1)) as usize;
    let row = ((y - g.geo.grid_top + g.scroll) / g.geo.cell_h.max(1)) as usize;
    if col >= g.geo.cols {
        return None;
    }
    let i = row * g.geo.cols + col;
    (i < g.items.len()).then_some(Spot::Item(i))
}

// ---- Fare ----

fn mouse_pos(lp: LPARAM) -> (i32, i32) {
    (
        (lp as u32 & 0xFFFF) as u16 as i16 as i32,
        ((lp as u32 >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

/// Fare konumu değişti (ya da içerik kaydı): vurguyu günceller.
fn set_hover(h: HWND, x: i32, y: i32) {
    let changed = with(|g| {
        let s = hit(g, x, y);
        let c = s != g.hover;
        g.hover = s;
        c
    })
    .unwrap_or(false);
    if changed {
        popup::redraw(h);
    }
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
            let track = with(|g| !std::mem::replace(&mut g.tracking, true)).unwrap_or(false);
            if track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: h,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
            }
            set_hover(h, x, y);
            return 0;
        }
        WM_MOUSELEAVE => {
            let changed = with(|g| {
                g.tracking = false;
                g.hover.take().is_some()
            })
            .unwrap_or(false);
            if changed {
                popup::redraw(h);
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16 as i32;
            let moved = with(|g| {
                let old = g.scroll;
                let step = g.geo.cell_h / 2;
                g.scroll = (g.scroll - delta * step / 120).clamp(0, g.geo.max_scroll);
                g.scroll != old
            })
            .unwrap_or(false);
            if moved {
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                ScreenToClient(h, &mut pt);
                with(|g| g.hover = None);
                set_hover(h, pt.x, pt.y);
                popup::redraw(h);
            }
            return 0;
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = mouse_pos(lp);
            let changed = with(|g| {
                g.down = hit(g, x, y);
                g.down.is_some()
            })
            .unwrap_or(false);
            if changed {
                popup::redraw(h);
            }
            return 0;
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_pos(lp);
            let act = with(|g| {
                let down = g.down.take()?;
                let up = hit(g, x, y)?;
                (down == up).then(|| match up {
                    Spot::Item(i) => g.items[i].path.clone(),
                    Spot::Reveal => g.dir.clone(),
                })
            })
            .flatten();
            match act {
                Some(path) => {
                    hide();
                    defer(move || stacks::open(&path));
                }
                None => popup::redraw(h),
            }
            return 0;
        }
        WM_RBUTTONUP => {
            let (x, y) = mouse_pos(lp);
            let path = with(|g| match hit(g, x, y)? {
                Spot::Item(i) => Some(g.items[i].path.clone()),
                Spot::Reveal => None,
            })
            .flatten();
            if let Some(path) = path {
                item_menu(path);
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

/// Öğe sağ tık menüsü. Menüye tıklamak ızgaranın dışına tıklamak sayılır ve onu kapatır.
fn item_menu(path: String) {
    crate::dock::hold_for_menu();
    let items = vec![MenuItem::item(CMD_OPEN, "Aç"), MenuItem::item(CMD_REVEAL, "Klasörde Göster")];
    let (x, y) = win::cursor_pos();
    shell_thread::show_menu(items, x, y, move |cmd| match cmd {
        CMD_OPEN => stacks::open(&path),
        CMD_REVEAL => crate::launch::show_in_folder(&path),
        _ => {}
    });
}

/// Dışarıda bir fare düğmesine basıldıysa ya da Esc'e basıldıysa kapatır.
fn poll_outside(h: HWND) {
    let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
    if down(VK_ESCAPE) {
        hide();
        return;
    }
    if shell_thread::menu_open() {
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

fn font(size: f32, weight: u32) -> *mut core::ffi::c_void {
    let face = win::wide("Segoe UI");
    unsafe {
        CreateFontW(
            -(size.round() as i32),
            0,
            0,
            0,
            weight as i32,
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
    }
}

fn draw_text(dc: HDC, text: &str, r: RECT, flags: u32) {
    let t = win::wide(text);
    let mut r = r;
    unsafe { DrawTextW(dc, t.as_ptr(), -1, &mut r, flags | DT_NOPREFIX) };
}

fn paint(h: HWND) {
    GRID.with(|g| {
        let Ok(g) = g.try_borrow() else { return };
        let Some(g) = g.as_ref() else { return };
        unsafe { paint_grid(h, g) };
    });
}

unsafe fn paint_grid(h: HWND, g: &Grid) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(h, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(h, &mut rc);
    let (w, hh) = (rc.right, rc.bottom);
    let geo = &g.geo;
    let dpi = g.dpi;
    let px = |v: f32| (v * dpi).round() as i32;

    // Görünür öğeler.
    let first = (g.scroll / geo.cell_h.max(1)) as usize * geo.cols;
    let visible: Vec<(usize, RECT)> = (first..g.items.len())
        .map(|i| (i, cell_rect(geo, g.scroll, i)))
        .take_while(|(_, r)| r.top < geo.grid_bottom)
        .collect();

    // 1) Şekiller SS kat büyük bir bitmap'e.
    let big = CreateCompatibleDC(hdc);
    let big_bmp = Gdi::select(big, CreateCompatibleBitmap(hdc, w * SS, hh * SS));
    let b = |v: i32| v * SS;
    let fill = |dc: HDC, color: u32, r: RECT, radius: i32| {
        let _b = Gdi::select(dc, CreateSolidBrush(cref(color)));
        let _p = Gdi::select(dc, CreatePen(PS_SOLID, 0, cref(color)));
        RoundRect(dc, b(r.left), b(r.top), b(r.right), b(r.bottom), radius * 2 * SS, radius * 2 * SS);
    };
    let bg = CreateSolidBrush(cref(BG));
    FillRect(big, &RECT { left: 0, top: 0, right: w * SS, bottom: hh * SS }, bg);
    DeleteObject(bg);

    IntersectClipRect(big, 0, b(geo.grid_top), b(w), b(geo.grid_bottom));
    let inset = px(3.0);
    for &(i, r) in &visible {
        let spot = Some(Spot::Item(i));
        if g.hover == spot {
            let color = if g.down == spot { PRESSED } else { HILITE };
            let r = RECT { left: r.left + inset, top: r.top + inset, right: r.right - inset, bottom: r.bottom - inset };
            fill(big, color, r, px(RADIUS));
        }
    }
    // Kaydırma çubuğu.
    if geo.max_scroll > 0 {
        let track = geo.grid_bottom - geo.grid_top;
        let total = track + geo.max_scroll;
        let len = (track * track / total).max(px(24.0));
        let top = geo.grid_top + (track - len) * g.scroll / geo.max_scroll;
        let bw = px(BAR_W);
        let l = w - geo.pad / 2 - bw / 2;
        fill(big, BAR, RECT { left: l, top, right: l + bw, bottom: top + len }, bw / 2);
    }

    // 2) Küçült (kenar yumuşatma).
    let small = CreateCompatibleDC(hdc);
    let small_bmp = Gdi::select(small, CreateCompatibleBitmap(hdc, w, hh));
    SetStretchBltMode(small, HALFTONE);
    SetBrushOrgEx(small, 0, 0, std::ptr::null_mut());
    StretchBlt(small, 0, 0, w, hh, big, 0, 0, w * SS, hh * SS, SRCCOPY);
    drop(big_bmp);
    DeleteDC(big);

    // 3) Başlık ve "Gezgin'de Aç".
    SetBkMode(small, TRANSPARENT as i32);
    let reveal = reveal_rect(geo, dpi);
    {
        let _f = Gdi::select(small, font(15.0 * dpi, FW_SEMIBOLD));
        SetTextColor(small, cref(TEXT));
        let r = RECT { left: geo.pad + px(4.0), top: geo.pad, right: reveal.left, bottom: reveal.bottom };
        draw_text(small, &g.title, r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
    }
    {
        let _f = Gdi::select(small, font(12.5 * dpi, FW_NORMAL));
        let hot = g.hover == Some(Spot::Reveal);
        SetTextColor(small, cref(if hot { g.accent } else { TEXT_DIM }));
        let r = RECT { right: reveal.right - px(4.0), ..reveal };
        draw_text(small, "Gezgin'de Aç ›", r, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
    }

    // 4) İkonlar ve adlar (ızgara alanına kırpılmış).
    IntersectClipRect(small, 0, geo.grid_top, w, geo.grid_bottom);
    if g.items.is_empty() {
        let _f = Gdi::select(small, font(13.0 * dpi, FW_NORMAL));
        SetTextColor(small, cref(TEXT_DIM));
        let r = RECT { left: 0, top: geo.grid_top, right: w, bottom: geo.grid_bottom };
        draw_text(small, "Klasör boş", r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
    let icon = px(ICON);
    let src = CreateCompatibleDC(hdc);
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    for &(i, r) in &visible {
        let Some(Some(d)) = g.icons.get(i) else { continue };
        // Küçük resim ikon kutusuna sığdırılır, alta hizalı ve yatayda ortalı.
        let k = (icon as f32 / d.w as f32).min(icon as f32 / d.h as f32).min(1.0);
        let (dw, dh) = (((d.w as f32 * k).round() as i32).max(1), ((d.h as f32 * k).round() as i32).max(1));
        let x = r.left + (geo.cell_w - dw) / 2;
        let y = r.top + px(ICON_TOP) + icon - dh;
        let old = SelectObject(src, d.bmp);
        if (dw, dh) != (d.w, d.h) {
            SetStretchBltMode(small, HALFTONE);
        }
        AlphaBlend(small, x, y, dw, dh, src, 0, 0, d.w, d.h, blend);
        SelectObject(src, old);
    }
    DeleteDC(src);
    {
        let _f = Gdi::select(small, font(12.0 * dpi, FW_NORMAL));
        SetTextColor(small, cref(TEXT));
        let side = px(TEXT_SIDE);
        for &(i, r) in &visible {
            let t = r.top + px(ICON_TOP) + icon + px(TEXT_GAP);
            let tr = RECT { left: r.left + side, top: t, right: r.right - side, bottom: r.bottom - px(4.0) };
            draw_text(small, &g.items[i].name, tr, DT_CENTER | DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS);
        }
    }

    BitBlt(hdc, 0, 0, w, hh, small, 0, 0, SRCCOPY);
    drop(small_bmp);
    DeleteDC(small);
    EndPaint(h, &ps);
}

