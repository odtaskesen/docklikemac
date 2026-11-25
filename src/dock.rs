//! Dock penceresinin durumu ve davranışları (ana / UI iş parçacığı).
//!
//! - Konum: ana ekranın altında ortalanmış; gövdenin altı ekranın altından `MARGIN` yukarıda.
//! - Yer ayırma: otomatik gizleme kapalıyken AppBar ile. Pencere gövdeden büyüktür
//!   (üstte büyüyen ikonlar için boşluk); ayrılan yer tüm pencereyi kapsar ki
//!   maximize pencereler saydam boşluğun altında kalmasın. Pencere bölgesi hiç
//!   değiştirilmez (fare girince değiştirmek tek karelik flaş yapıyordu).
//! - Cam modu: Acrylic ayrı bir arka plan penceresinde, gövdenin tam altında.
//! - Otomatik gizleme: fare ayrılınca aşağı kayar; alt kenardaki görünmez şeride
//!   gelince yukarı çıkar. Sadece animasyon sırasında zamanlayıcı çalışır.
//! - Tam ekran uygulama açılınca gizlenir (ABN_FULLSCREENAPP).

use std::cell::RefCell;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer, TimerMode};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::config::{Appearance, Config};
use crate::shell_thread::{self, MenuItem};
use crate::{appbar, apps, size_panel, taskbar, widgets, win, DockWindow, Z};

/// Gövdenin ekranın alt kenarına uzaklığı (mantıksal piksel).
const MARGIN: f32 = 8.0;
/// Köşe yarıçapı (mantıksal piksel), `dock.slint` ile aynı.
const RADIUS: f32 = 16.0;
const ANIM: Duration = Duration::from_millis(180);
const ANIM_FRAME: Duration = Duration::from_millis(15);
/// Büyüme: fare girince bekleme, büyüme ve küçülme süreleri.
const MAG_DELAY: Duration = Duration::from_millis(80);
const MAG_IN: Duration = Duration::from_millis(300);
const MAG_OUT: Duration = Duration::from_millis(200);

// Dock menüsü komutları.
const ID_APPEARANCE: u32 = 100; // 100, 101, 102
const ID_AUTO_HIDE: u32 = 110;
const ID_QUIT: u32 = 111;
const ID_MAGNIFY: u32 = 112;
const ID_STARTUP: u32 = 113;
const ID_SIZE: u32 = 114;
const ID_SETTINGS: u32 = 115;

/// Run anahtarındaki değer adı.
const STARTUP_NAME: &str = "Dock";

/// Run anahtarına yazılacak komut: bu exe'nin tırnaklı yolu.
fn startup_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

/// "Windows ile Başlat"ı açar/kapatır.
pub fn set_startup(on: bool) -> bool {
    match startup_command() {
        Some(cmd) => {
            let ok = crate::launch::set_startup(STARTUP_NAME, &cmd, on);
            eprintln!("[başlangıç] {} {cmd}: {}", if on { "açıldı" } else { "kapatıldı" }, if ok { "tamam" } else { "hata" });
            ok
        }
        None => false,
    }
}

struct Dock {
    ui: slint::Weak<DockWindow>,
    hwnd: HWND,
    backdrop: HWND,
    cfg: Config,
    scale: f32,
    /// Hedef durum: dock görünür mü (otomatik gizleme / tam ekran hariç).
    visible: bool,
    fullscreen: bool,
    last_size: (i32, i32),
    backdrop_size: (i32, i32),
    hide_timer: Timer,
    anim_timer: Timer,
    anim_from: i32,
    anim_to: i32,
    anim_start: Instant,
    anim_hide_at_end: bool,
    mag: f32,
    mag_from: f32,
    mag_to: f32,
    mag_start: Instant,
    mag_delay: Duration,
    mag_dur: Duration,
    mag_timer: Timer,
    /// Ayarlar penceresindeki kaydırıcılar için gecikmeli kaydetme.
    save_timer: Timer,
}

thread_local! {
    static STATE: RefCell<Option<Dock>> = const { RefCell::new(None) };
}

/// Durumu ödünç alır. Zaten ödünçteyse (yeniden giriş) sessizce atlar.
fn with<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    STATE.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        s.as_mut().map(f)
    })
}

struct Geo {
    x: i32,
    w: i32,
    shown_y: i32,
    hidden_y: i32,
}

impl Dock {
    fn px(&self, logical: f32) -> i32 {
        (logical * self.scale).round() as i32
    }

    /// Dock boyutu ölçeğiyle çarpılmış mantıksal ölçüyü fiziksel piksele çevirir.
    fn zpx(&self, logical: f32) -> i32 {
        self.px(logical * self.cfg.scale)
    }

    fn margin(&self) -> i32 {
        self.zpx(MARGIN)
    }

    fn geo(&self) -> Geo {
        let (w, h) = win::window_size(self.hwnd);
        let m = appbar::primary_monitor_rect();
        Geo {
            x: m.left + (m.right - m.left - w) / 2,
            w,
            shown_y: m.bottom - self.margin() - h,
            hidden_y: m.bottom,
        }
    }

    fn reserve(&self) -> i32 {
        if self.cfg.auto_hide {
            0
        } else {
            // Pencerenin tamamı (gövde + büyüme boşluğu) + alttaki boşluk.
            win::window_size(self.hwnd).1 + self.margin()
        }
    }

    /// Yer ayırmayı ve konumu günceller.
    fn layout(&mut self) {
        appbar::set_pos(self.reserve());
        if self.anim_timer.running() {
            return;
        }
        let g = self.geo();
        self.place(g.x, if self.visible { g.shown_y } else { g.hidden_y });
        if !self.visible && self.cfg.auto_hide && !self.fullscreen {
            show_edge(&g);
        }
    }

    /// Dock penceresini taşır, cam arka planını da beraberinde götürür.
    fn place(&mut self, x: i32, y: i32) {
        win::move_to(self.hwnd, x, y);
        self.sync_backdrop();
    }

    /// Cam arka planını gövdenin altına hizalar (cam modunda ve dock görünürken).
    fn sync_backdrop(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        if self.cfg.appearance != Appearance::Cam || !win::is_visible(self.hwnd) {
            win::hide(self.backdrop);
            return;
        }
        let r = win::window_rect(self.hwnd);
        let x = r.left + self.px(ui.get_body_x());
        let y = r.top + self.px(ui.get_body_y());
        let w = self.px(ui.get_body_w());
        let h = self.px(ui.get_body_h());
        if (w, h) != self.backdrop_size {
            self.backdrop_size = (w, h);
            win::set_region(self.backdrop, Some((0, 0, w, h)), self.zpx(RADIUS));
        }
        win::place_below(self.backdrop, self.hwnd, x, y, w, h);
    }

    fn apply_appearance(&mut self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_mode(self.cfg.appearance.index());
            ui.set_magnify_enabled(self.cfg.magnify);
            ui.global::<Z>().set_m(self.cfg.magnify_max);
        }
        self.sync_backdrop();
    }

    fn schedule_hide(&mut self, ms: u64) {
        if !self.cfg.auto_hide || self.fullscreen || !self.visible {
            return;
        }
        self.hide_timer.start(TimerMode::SingleShot, Duration::from_millis(ms), || {
            with(|d| d.try_hide());
        });
    }

    fn try_hide(&mut self) {
        if !self.cfg.auto_hide || self.fullscreen || !self.visible {
            return;
        }
        if shell_thread::menu_open()
            || size_panel::is_open()
            || crate::preview::is_open()
            || crate::stack_grid::is_open()
            || win::cursor_inside(self.hwnd)
        {
            return; // fare ayrılınca WM_MOUSELEAVE tekrar planlar
        }
        self.slide_out();
    }

    fn slide_out(&mut self) {
        self.visible = false;
        let g = self.geo();
        self.animate(g.hidden_y, true);
    }

    fn slide_in(&mut self) {
        shell_thread::post(shell_thread::MSG_EDGE_HIDE, 0, 0);
        let g = self.geo();
        if !self.visible && !self.anim_timer.running() {
            win::move_to(self.hwnd, g.x, g.hidden_y);
        }
        win::show_no_activate(self.hwnd);
        widgets::set_visible(true);
        self.visible = true;
        self.animate(g.shown_y, false);
    }

    fn animate(&mut self, to: i32, hide_at_end: bool) {
        self.anim_from = win::window_rect(self.hwnd).top;
        self.anim_to = to;
        self.anim_start = Instant::now();
        self.anim_hide_at_end = hide_at_end;
        self.anim_timer.start(TimerMode::Repeated, ANIM_FRAME, || {
            with(|d| d.anim_tick());
        });
    }

    fn anim_tick(&mut self) {
        let t = (self.anim_start.elapsed().as_secs_f32() / ANIM.as_secs_f32()).min(1.0);
        let e = 1.0 - (1.0 - t).powi(3); // ease-out
        let y = self.anim_from + ((self.anim_to - self.anim_from) as f32 * e).round() as i32;
        let g = self.geo();
        self.place(g.x, y);
        if t >= 1.0 {
            self.anim_timer.stop();
            if self.anim_hide_at_end {
                win::hide(self.hwnd);
                widgets::set_visible(false);
                self.sync_backdrop();
                if self.cfg.auto_hide && !self.fullscreen {
                    show_edge(&g);
                }
            }
        }
    }

    fn mag_tick(&mut self) {
        let elapsed = self.mag_start.elapsed();
        if elapsed < self.mag_delay {
            return;
        }
        let t = ((elapsed - self.mag_delay).as_secs_f32() / self.mag_dur.as_secs_f32()).min(1.0);
        // ease-in-out (yavaş başla, hızlan, yavaş bitir)
        let e = if t < 0.5 { 2.0 * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(2) / 2.0 };
        self.mag = self.mag_from + (self.mag_to - self.mag_from) * e;
        if let Some(ui) = self.ui.upgrade() {
            ui.set_mag(self.mag);
        }
        if t >= 1.0 {
            self.mag_timer.stop();
        }
    }

    fn apply_auto_hide(&mut self) {
        appbar::set_pos(self.reserve());
        if self.cfg.auto_hide {
            self.schedule_hide(600);
        } else {
            self.hide_timer.stop();
            if !self.fullscreen {
                self.slide_in();
            }
        }
    }

    fn menu(&self) -> Vec<MenuItem> {
        let a = self.cfg.appearance;
        vec![
            MenuItem::sub(
                "Görünüm",
                vec![
                    MenuItem::check(ID_APPEARANCE, "Şeffaf", a == Appearance::Seffaf),
                    MenuItem::check(ID_APPEARANCE + 1, "Cam", a == Appearance::Cam),
                    MenuItem::check(ID_APPEARANCE + 2, "Koyu", a == Appearance::Koyu),
                ],
            ),
            MenuItem::item(
                ID_AUTO_HIDE,
                if self.cfg.auto_hide { "Otomatik Gizlemeyi Kapat" } else { "Otomatik Gizlemeyi Aç" },
            ),
            MenuItem::item(
                ID_MAGNIFY,
                if self.cfg.magnify { "Büyütmeyi Kapat" } else { "Büyütmeyi Aç" },
            ),
            MenuItem::item(ID_SIZE, "Boyut…"),
            MenuItem::item(ID_SETTINGS, "Ayarlar…"),
            MenuItem::check(
                ID_STARTUP,
                "Windows ile Başlat",
                crate::launch::startup_enabled(STARTUP_NAME),
            ),
            MenuItem::Separator,
            MenuItem::item(ID_QUIT, "Çıkış"),
        ]
    }
}

impl Dock {
    /// Dock boyutunu canlı değiştirir. Pencere boyutu Slint'te değişir; alt
    /// sınıf yeni boyutu alt-ortaya sabitler (`win.rs`), `on_resized` yerleştirir.
    fn set_zoom(&mut self, z: f32) {
        let z = z.clamp(size_panel::MIN, size_panel::MAX);
        if (z - self.cfg.scale).abs() < f32::EPSILON {
            return;
        }
        self.cfg.scale = z;
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<Z>().set_k(z);
        }
    }

    /// Boyut panelinin yeri: dock penceresinin üstünde, yatayda ortalı.
    fn panel_anchor(&self) -> (i32, i32) {
        let g = self.geo();
        (g.x + g.w / 2, g.shown_y - self.px(size_panel::GAP))
    }

    fn open_size_panel(&mut self) {
        self.hide_timer.stop();
        let (cx, bottom) = self.panel_anchor();
        size_panel::open(
            size_panel::Kind::Size,
            self.cfg.scale,
            self.scale,
            crate::launch::accent_color(),
            cx,
            bottom,
            |z| {
                with(|d| d.set_zoom(z));
            },
            |z| {
                with(|d| {
                    d.set_zoom(z);
                    d.cfg.save();
                    d.schedule_hide(500);
                });
            },
        );
    }
}

/// Ses panelini imlecin üstünde (dock penceresinin üstünde) açar ya da açık tutar.
pub fn open_volume_panel(level: f32) {
    with(|d| {
        if size_panel::kind() == Some(size_panel::Kind::Volume) {
            size_panel::touch();
            return;
        }
        d.hide_timer.stop();
        let (_, bottom) = d.panel_anchor();
        let (cx, _) = win::cursor_pos();
        size_panel::open(
            size_panel::Kind::Volume,
            level,
            d.scale,
            crate::launch::accent_color(),
            cx,
            bottom,
            crate::sysstatus::set_volume,
            |_| {
                with(|d| d.schedule_hide(500));
            },
        );
    });
}

fn show_edge(g: &Geo) {
    shell_thread::post(shell_thread::MSG_EDGE_SHOW, g.x as u32 as usize, g.w as isize);
}

// ---- Dışarıdan çağrılanlar (hepsi UI iş parçacığında) ----

pub fn init(ui: &DockWindow, hwnd: HWND) {
    let mut cfg = Config::load();
    sanitize(&mut cfg);
    ui.global::<Z>().set_k(cfg.scale);
    // Varsayılan: Windows ile başlat (bir kez). Debug exe Run anahtarına yazılmaz.
    if !cfg.startup_initialized && !cfg!(debug_assertions) {
        set_startup(true);
        cfg.startup_initialized = true;
        cfg.save();
    }
    win::make_tool_window(hwnd);
    win::subclass(hwnd);
    win::accept_files(hwnd);
    taskbar::hide();
    appbar::register(hwnd);

    let pinned = cfg.pinned.clone();
    let pins_imported = cfg.pins_imported;
    let dock = Dock {
        ui: ui.as_weak(),
        hwnd,
        backdrop: win::create_backdrop(),
        cfg,
        scale: ui.window().scale_factor(),
        visible: true,
        fullscreen: false,
        last_size: win::window_size(hwnd),
        backdrop_size: (0, 0),
        hide_timer: Timer::default(),
        anim_timer: Timer::default(),
        anim_from: 0,
        anim_to: 0,
        anim_start: Instant::now(),
        anim_hide_at_end: false,
        mag: 0.0,
        mag_from: 0.0,
        mag_to: 0.0,
        mag_start: Instant::now(),
        mag_delay: Duration::ZERO,
        mag_dur: MAG_IN,
        mag_timer: Timer::default(),
        save_timer: Timer::default(),
    };
    STATE.with(|s| *s.borrow_mut() = Some(dock));
    with(|d| {
        d.apply_appearance();
        d.layout();
        // Açılışta bir an görünsün, sonra gizlensin.
        d.schedule_hide(1500);
    });
    apps::init(ui, pinned, pins_imported);
    widgets::init(ui);
}

pub fn startup_enabled() -> bool {
    crate::launch::startup_enabled(STARTUP_NAME)
}

/// Ayar değerlerini geçerli aralıklara çeker.
fn sanitize(cfg: &mut Config) {
    use crate::config::{MAG_DEFAULT, MAG_MAX, MAG_MIN};
    cfg.scale = if cfg.scale.is_finite() { cfg.scale.clamp(size_panel::MIN, size_panel::MAX) } else { 1.0 };
    cfg.magnify_max =
        if cfg.magnify_max.is_finite() { cfg.magnify_max.clamp(MAG_MIN, MAG_MAX) } else { MAG_DEFAULT };
}

/// Ayarların kopyası (Ayarlar penceresi için).
pub fn config() -> Option<Config> {
    with(|d| d.cfg.clone())
}

pub fn widgets_cfg() -> crate::config::WidgetsCfg {
    with(|d| d.cfg.widgets).unwrap_or_default()
}

/// Ayarlar penceresinden gelen değişikliği uygular. Kayıt kısa bir gecikmeyle
/// yapılır (kaydırıcı sürüklenirken her adımda dosya yazılmasın).
pub fn update_config(f: impl FnOnce(&mut Config)) {
    let Some(widgets) = with(|d| {
        let old = d.cfg.clone();
        f(&mut d.cfg);
        sanitize(&mut d.cfg);
        if d.cfg.scale != old.scale {
            if let Some(ui) = d.ui.upgrade() {
                ui.global::<Z>().set_k(d.cfg.scale);
            }
        }
        d.apply_appearance();
        if d.cfg.auto_hide != old.auto_hide {
            d.apply_auto_hide();
        }
        d.save_timer.start(TimerMode::SingleShot, Duration::from_millis(500), || {
            with(|d| d.cfg.save());
        });
        d.cfg.widgets
    }) else {
        return;
    };
    widgets::apply_config(widgets);
}

/// Bekleyen gecikmeli kaydı hemen yapar.
pub fn flush_save() {
    with(|d| {
        if d.save_timer.running() {
            d.save_timer.stop();
            d.cfg.save();
        }
    });
}

/// Dock şu an ekranda mı (otomatik gizleme / tam ekran nedeniyle gizli değil).
pub fn shown() -> bool {
    with(|d| d.visible && !d.fullscreen).unwrap_or(true)
}

pub fn scale() -> f32 {
    with(|d| d.scale).unwrap_or(1.0)
}

/// Sabitlenmiş uygulama listesini kaydeder.
pub fn save_pinned(pinned: Vec<crate::config::PinnedApp>) {
    with(|d| {
        d.cfg.pinned = pinned;
        d.cfg.pins_imported = true;
        d.cfg.save();
    });
}

/// Yığın klasörleri (5f).
pub fn stacks() -> Vec<String> {
    with(|d| d.cfg.stacks.clone()).unwrap_or_default()
}

pub fn save_stacks(stacks: Vec<String>) {
    with(|d| {
        d.cfg.stacks = stacks;
        d.cfg.save();
    });
}

pub fn toggle_auto_hide() {
    with(|d| {
        d.cfg.auto_hide = !d.cfg.auto_hide;
        d.cfg.save();
        d.apply_auto_hide();
    });
}

pub fn on_context_menu() {
    let Some(items) = with(|d| {
        d.hide_timer.stop();
        d.menu()
    }) else {
        return;
    };
    let (x, y) = win::cursor_pos();
    shell_thread::show_menu(items, x, y, on_menu_command);
}

fn on_menu_command(cmd: u32) {
    match cmd {
        c if (ID_APPEARANCE..ID_APPEARANCE + 3).contains(&c) => {
            with(|d| {
                d.cfg.appearance = Appearance::from_index(c - ID_APPEARANCE);
                d.cfg.save();
                d.apply_appearance();
            });
        }
        ID_AUTO_HIDE => toggle_auto_hide(),
        ID_MAGNIFY => {
            with(|d| {
                d.cfg.magnify = !d.cfg.magnify;
                d.cfg.save();
                d.apply_appearance();
            });
        }
        ID_SIZE => {
            with(|d| d.open_size_panel());
        }
        ID_SETTINGS => crate::settings::open(),
        ID_STARTUP => {
            set_startup(!crate::launch::startup_enabled(STARTUP_NAME));
        }
        ID_QUIT => {
            let _ = slint::quit_event_loop();
        }
        _ => {}
    }
}

/// Herhangi bir menü kapandıktan sonra (seçim ya da iptal).
pub fn on_menu_closed() {
    with(|d| d.schedule_hide(500));
}

/// Uygulama menüsü açılırken otomatik gizleme beklesin.
pub fn hold_for_menu() {
    with(|d| d.hide_timer.stop());
}

pub fn on_mouse_leave() {
    crate::apps::on_dock_leave();
    with(|d| d.schedule_hide(400));
}

/// Dock üstü açılır pencere için yer: ikon merkezinin (mantıksal, pencereye göre)
/// fiziksel x'i, pencerenin altının oturacağı y ve ekran ölçeği.
pub fn popup_anchor(center: f32) -> Option<(i32, i32, f32)> {
    with(|d| {
        let g = d.geo();
        (g.x + d.px(center), g.shown_y - d.px(size_panel::GAP), d.scale)
    })
}

/// Büyüme geçişini başlatır: girişte kısa bir bekleme ve yavaş başlayıp
/// hızlanan bir artış; çıkışta daha kısa bir azalış.
pub fn on_mag_target(on: bool) {
    with(|d| {
        let to = if on { 1.0 } else { 0.0 };
        if d.mag_to == to && (d.mag_timer.running() || d.mag == to) {
            return;
        }
        d.mag_from = d.mag;
        d.mag_to = to;
        d.mag_start = Instant::now();
        let span = (to - d.mag).abs().max(0.2);
        if on {
            d.mag_delay = MAG_DELAY;
            d.mag_dur = MAG_IN.mul_f32(span);
        } else {
            d.mag_delay = Duration::ZERO;
            d.mag_dur = MAG_OUT.mul_f32(span);
        }
        d.mag_timer.start(TimerMode::Repeated, ANIM_FRAME, || {
            with(|d| d.mag_tick());
        });
    });
}

pub fn on_body_changed() {
    with(|d| d.sync_backdrop());
}

pub fn on_edge_hit() {
    with(|d| {
        if d.fullscreen || !d.cfg.auto_hide {
            return;
        }
        d.hide_timer.stop();
        d.slide_in();
        // İmleç dock'a girmezse kendiliğinden geri insin.
        d.schedule_hide(1500);
    });
}

pub fn on_fullscreen(on: bool) {
    // Win+D (masaüstünü göster) de bu bildirimi tetikleyebiliyor; onu yok say.
    if on {
        let cls = win::class_name(unsafe { GetForegroundWindow() });
        if cls == "WorkerW" || cls == "Progman" {
            return;
        }
    }
    with(|d| {
        if d.fullscreen == on {
            return;
        }
        d.fullscreen = on;
        if on {
            d.anim_timer.stop();
            d.hide_timer.stop();
            size_panel::close();
            crate::apps::hide_preview();
            crate::stack_grid::hide();
            win::hide(d.hwnd);
            widgets::set_visible(false);
            d.sync_backdrop();
            shell_thread::post(shell_thread::MSG_EDGE_HIDE, 0, 0);
            d.visible = false;
        } else if d.cfg.auto_hide {
            d.layout(); // gizli konumda kalır, şeridi gösterir
        } else {
            d.visible = true;
            win::show_no_activate(d.hwnd);
            widgets::set_visible(true);
            d.layout();
        }
    });
}

pub fn on_resized() {
    with(|d| {
        let size = win::window_size(d.hwnd);
        if size != d.last_size {
            d.last_size = size;
            if let Some(ui) = d.ui.upgrade() {
                d.scale = ui.window().scale_factor();
            }
            d.layout();
            if size_panel::kind() == Some(size_panel::Kind::Size) {
                let (cx, bottom) = d.panel_anchor();
                size_panel::move_to(cx, bottom);
            }
        }
    });
}

pub fn on_display_changed() {
    with(|d| d.layout());
}

pub fn on_explorer_restarted() {
    eprintln!("[explorer] yeniden başladı");
    with(|d| {
        taskbar::hide();
        appbar::reregister(d.hwnd);
        d.layout();
    });
    shell_thread::post(shell_thread::MSG_REHOOK, 0, 0);
}
