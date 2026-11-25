//! Widget'lar ve sistem alanı: verileri modüllerden toplar, arayüze aktarır.
//!
//! - Müzik, ağ, ses, hava durumu: olay tabanlı (modüller değişince bildirir).
//! - Pil: dock penceresine gelen WM_POWERBROADCAST ile.
//! - CPU/RAM: 2 sn'de bir, sadece dock görünürken.
//! - Müzik ilerleme çubuğu: 1 sn'de bir, sadece çalarken ve dock görünürken.
//! - Saat: dakika başlarında.

use std::cell::RefCell;
use std::time::Duration;

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

use crate::config::WidgetsCfg;
use crate::shell_thread::{self, MenuItem};
use crate::{media, recycle, sysstatus, weather, win, DockWindow, MediaInfo, SysInfo};

// widgets.slint `WidgetId` ile aynı.
const ID_MEDIA_PREV: i32 = 1;
const ID_MEDIA_TOGGLE: i32 = 2;
const ID_MEDIA_NEXT: i32 = 3;
const ID_TRAY: i32 = 10;
const ID_QUICK: i32 = 11;
const ID_CLOCK: i32 = 12;
const ID_BELL: i32 = 13;
const ID_TRASH: i32 = 20;
/// Yığınlar: `ID_STACK + sıra`.
const ID_STACK: i32 = 30;

// Çöp kutusu menüsü komutları.
const CMD_TRASH_OPEN: u32 = 1;
const CMD_TRASH_EMPTY: u32 = 2;
// Yığın menüsü komutları.
const CMD_STACK_REVEAL: u32 = 1;
const CMD_STACK_REMOVE: u32 = 2;
/// Izgarada en fazla bu kadar öğe (en yeniler).
const STACK_LIMIT: usize = 300;

const DAYS: [&str; 7] = ["Paz", "Pzt", "Sal", "Çar", "Per", "Cum", "Cmt"];
const MONTHS: [&str; 12] = [
    "Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara",
];

struct Widgets {
    ui: slint::Weak<DockWindow>,
    media: media::MediaState,
    art: Option<Image>,
    sys: SysInfo,
    sampler: sysstatus::CpuSampler,
    stats_timer: Timer,
    progress_timer: Timer,
    clock_timer: Timer,
    visible: bool,
    /// Son bilinen ses seviyesi (0..1), tekerlek ve ses paneli için.
    volume: f32,
    /// Geri Dönüşüm Kutusu dolu mu (izleyici bildirir).
    trash_full: bool,
    /// Yığın klasörleri (config sırası) ve dock ikonları.
    stacks: Vec<String>,
    stack_icons: Vec<Image>,
    /// Ayarlardan açık/kapalı widget'lar (kapalıysa zamanlayıcısı da durur).
    cfg: WidgetsCfg,
}

thread_local! {
    static STATE: RefCell<Option<Widgets>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Widgets) -> R) -> Option<R> {
    STATE.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        s.as_mut().map(f)
    })
}

fn defer(f: impl FnOnce() + Send + 'static) {
    let _ = slint::invoke_from_event_loop(f);
}

impl Widgets {
    fn push_sys(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_sys(self.sys.clone());
        }
    }

    fn push_media(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let m = &self.media;
        let mut pos = m.position_ms;
        if m.playing {
            if let Some(t) = m.updated_at {
                pos += t.elapsed().as_millis() as u64;
            }
        }
        let has_progress = m.duration_ms > 0;
        let progress = if has_progress { (pos as f32 / m.duration_ms as f32).min(1.0) } else { 0.0 };
        ui.set_media(MediaInfo {
            active: m.active,
            title: m.title.clone().into(),
            artist: m.artist.clone().into(),
            playing: m.playing,
            progress,
            has_progress,
            art: self.art.clone().unwrap_or_default(),
            has_art: self.art.is_some(),
            can_prev: m.can_prev,
            can_next: m.can_next,
        });
    }

    /// Zamanlayıcıları görünürlüğe ve çalma durumuna göre aç/kapat.
    fn update_timers(&mut self) {
        if self.visible && self.cfg.stats {
            if !self.stats_timer.running() {
                self.stats_timer.start(TimerMode::Repeated, Duration::from_secs(2), || {
                    with(|w| w.sample_stats());
                });
            }
        } else {
            self.stats_timer.stop();
        }

        let need_progress = self.visible && self.cfg.music && self.media.active && self.media.playing && self.media.duration_ms > 0;
        if need_progress {
            if !self.progress_timer.running() {
                self.progress_timer.start(TimerMode::Repeated, Duration::from_secs(1), || {
                    with(|w| w.push_media());
                });
            }
        } else {
            self.progress_timer.stop();
        }
    }

    fn push_cfg(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let c = self.cfg;
        ui.set_w_music(c.music);
        ui.set_w_weather(c.weather);
        ui.set_w_stats(c.stats);
        ui.set_w_system(c.system);
        ui.set_w_clock(c.clock);
        ui.set_w_trash(c.trash);
    }

    fn sample_stats(&mut self) {
        let s = self.sampler.sample();
        self.sys.cpu = s.cpu;
        self.sys.ram = s.ram;
        self.push_sys();
    }

    fn update_clock(&mut self) {
        let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut t) };
        self.sys.time = format!("{:02}:{:02}", t.wHour, t.wMinute).into();
        self.sys.date = format!(
            "{}, {} {}",
            DAYS[(t.wDayOfWeek as usize) % 7],
            t.wDay,
            MONTHS[(t.wMonth as usize).clamp(1, 12) - 1]
        )
        .into();
        self.push_sys();
        // Bir sonraki dakika başına kur.
        let ms = (60 - t.wSecond as u64) * 1000 - t.wMilliseconds as u64 + 50;
        self.clock_timer.start(TimerMode::SingleShot, Duration::from_millis(ms), || {
            with(|w| w.update_clock());
        });
    }

    fn update_battery(&mut self) {
        let b = sysstatus::battery();
        self.sys.battery = b.present;
        self.sys.battery_percent = b.percent as i32;
        self.sys.charging = b.charging;
        self.push_sys();
    }
}

fn net_icon(n: &sysstatus::NetState) -> &'static str {
    if !n.connected {
        "no-network"
    } else if n.wifi {
        ["wifi-0", "wifi-1", "wifi-2", "wifi-3", "wifi-4"][n.signal.min(4) as usize]
    } else {
        "ethernet"
    }
}

fn volume_icon(v: &sysstatus::VolumeState) -> &'static str {
    if v.muted {
        "volume-mute"
    } else if v.level < 0.01 {
        "volume-0"
    } else if v.level < 0.34 {
        "volume-1"
    } else if v.level < 0.67 {
        "volume-2"
    } else {
        "volume-3"
    }
}

/// Tray taşma menüsü: görev çubuğu gizliyken de çalışan yöntem (0. aşamada doğrulandı).
fn open_tray() {
    std::thread::spawn(|| {
        win::send_combo(&[win::KEY_WIN, win::KEY_B]);
        std::thread::sleep(Duration::from_millis(200));
        win::send_combo(&[win::KEY_ENTER]);
    });
}

// ---- Dışarıdan çağrılanlar (UI iş parçacığı) ----

pub fn init(ui: &DockWindow) {
    let w = Widgets {
        ui: ui.as_weak(),
        media: media::MediaState::default(),
        art: None,
        sys: SysInfo {
            net_icon: "ethernet".into(),
            volume_icon: "volume-2".into(),
            ..Default::default()
        },
        sampler: sysstatus::CpuSampler::new(),
        stats_timer: Timer::default(),
        progress_timer: Timer::default(),
        clock_timer: Timer::default(),
        visible: true,
        volume: 0.5,
        trash_full: false,
        stacks: crate::dock::stacks(),
        stack_icons: Vec::new(),
        cfg: crate::dock::widgets_cfg(),
    };
    STATE.with(|s| *s.borrow_mut() = Some(w));
    with(|w| {
        w.push_cfg();
        w.update_clock();
        w.update_battery();
        w.sample_stats();
        w.update_timers();
        w.push_media();
    });

    weather::start(|data| defer(move || on_weather(data)));
    sysstatus::start_network(|n| defer(move || on_network(n)));
    sysstatus::start_volume(|v| defer(move || on_volume(v)));
    media::start(
        |m| defer(move || on_media(m)),
        |art| defer(move || on_art(art)),
    );
    recycle::start(|full| defer(move || on_trash(full)));
    load_stack_icons();
}

// ---- Yığınlar (5f) ----

/// Yığın klasörlerinin ikonlarını arka planda yükler, gelince dock'a aktarır.
fn load_stack_icons() {
    let Some(dirs) = with(|w| {
        w.stack_icons = w.stacks.iter().map(|_| Image::default()).collect();
        w.push_stacks();
        w.stacks.clone()
    }) else {
        return;
    };
    let size = crate::apps::icon_size_for(crate::dock::scale());
    let _ = std::thread::Builder::new().name("stack-dock-icons".into()).spawn(move || {
        crate::icons::com_init_thread();
        for dir in dirs {
            // Açılışta kabuk bazen ikonu veremiyor (Gezgin'den başlatınca görüldü): birkaç kez dene.
            let mut got = None;
            for attempt in 0..6 {
                got = crate::icons::load_icon(&dir, size);
                if got.is_some() {
                    break;
                }
                eprintln!("[yığın] ikon alınamadı ({}. deneme): {dir}", attempt + 1);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            if let Some(p) = got {
                defer(move || {
                    with(|w| {
                        let Some(i) = w.stacks.iter().position(|d| *d == dir) else { return };
                        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&p.rgba, p.width, p.height);
                        w.stack_icons[i] = Image::from_rgba8(buf);
                        w.push_stacks();
                    });
                });
            }
        }
    });
}

impl Widgets {
    fn push_stacks(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_stacks(std::rc::Rc::new(slint::VecModel::from(self.stack_icons.clone())).into());
        }
    }
}

/// Yığına tık: ızgarayı açar; açıksa kapatır.
fn open_stack(i: usize) {
    let Some(Some((dir, center, ui))) = with(|w| {
        let dir = w.stacks.get(i)?.clone();
        let ui = w.ui.upgrade()?;
        // Yığın hücreleri çöp kutusunun hemen solunda.
        let k = ui.global::<crate::Z>().get_k();
        let (base, spacing) = (44.0 * k, 6.0 * k);
        let n = w.stacks.len() as f32;
        let center = ui.get_trash_left() - (n - i as f32) * (base + spacing) + base / 2.0;
        Some((dir, center, ui))
    }) else {
        return;
    };
    if crate::stack_grid::is_open_for(&dir) {
        crate::stack_grid::hide();
        return;
    }
    // Dock'a basınca dışarı tıklama yoklaması ızgarayı kapattı: bu tık yeniden açmasın.
    if crate::stack_grid::just_closed(&dir) {
        return;
    }
    crate::apps::hide_preview();
    let Some((cx, bottom, dpi)) = crate::dock::popup_anchor(center) else { return };
    let items = crate::stacks::list(&dir, STACK_LIMIT)
        .into_iter()
        .map(|e| crate::stack_grid::Item { name: e.name, path: e.path })
        .collect();
    let title = crate::stacks::display_name(&dir);
    crate::stack_grid::show(dir, title, items, cx, bottom, dpi, crate::launch::accent_color());
    ui.set_stack_open(i as i32);
}

/// Izgara kapandı.
pub fn on_stack_closed() {
    with(|w| {
        if let Some(ui) = w.ui.upgrade() {
            ui.set_stack_open(-1);
        }
    });
}

/// Yığın sağ tıkı: Gezgin'de Aç / Dock'tan Kaldır.
fn stack_menu(i: usize) {
    let Some(Some(dir)) = with(|w| w.stacks.get(i).cloned()) else { return };
    crate::stack_grid::hide();
    crate::dock::hold_for_menu();
    let items = vec![
        MenuItem::item(CMD_STACK_REVEAL, "Gezgin'de Aç"),
        MenuItem::Separator,
        MenuItem::item(CMD_STACK_REMOVE, "Dock'tan Kaldır"),
    ];
    let (x, y) = win::cursor_pos();
    shell_thread::show_menu(items, x, y, move |cmd| match cmd {
        CMD_STACK_REVEAL => crate::stacks::open(&dir),
        CMD_STACK_REMOVE => {
            with(|w| {
                if let Some(i) = w.stacks.iter().position(|d| *d == dir) {
                    w.stacks.remove(i);
                    w.stack_icons.remove(i);
                    w.push_stacks();
                    crate::dock::save_stacks(w.stacks.clone());
                }
            });
        }
        _ => {}
    });
}

/// Dock'a bırakılan klasörler yığın olarak eklenir.
fn add_stacks(dirs: Vec<String>) {
    let added = with(|w| {
        let mut added = false;
        for d in dirs {
            if !w.stacks.iter().any(|s| s.eq_ignore_ascii_case(&d)) {
                w.stacks.push(d);
                added = true;
            }
        }
        if added {
            crate::dock::save_stacks(w.stacks.clone());
        }
        added
    })
    .unwrap_or(false);
    if added {
        load_stack_icons();
    }
}

fn on_trash(full: bool) {
    with(|w| {
        w.trash_full = full;
        if let Some(ui) = w.ui.upgrade() {
            ui.set_trash_full(full);
        }
    });
}

/// Widget sağ tıkı. Şimdilik sadece çöp kutusu: Aç / Boşalt (Windows onay sorar).
pub fn on_menu(id: i32) {
    if id >= ID_STACK {
        stack_menu((id - ID_STACK) as usize);
        return;
    }
    if id != ID_TRASH {
        return;
    }
    let full = with(|w| w.trash_full).unwrap_or(true);
    crate::dock::hold_for_menu();
    let items = vec![
        MenuItem::item(CMD_TRASH_OPEN, "Aç"),
        MenuItem::Separator,
        MenuItem::Item { id: CMD_TRASH_EMPTY, text: "Boşalt…".into(), checked: false, enabled: full },
    ];
    let (x, y) = win::cursor_pos();
    shell_thread::show_menu(items, x, y, |cmd| match cmd {
        CMD_TRASH_OPEN => recycle::open(),
        CMD_TRASH_EMPTY => recycle::empty(),
        _ => {}
    });
}

/// Explorer'dan bırakılan dosyalar. `x` dock penceresinin istemci koordinatı
/// (fiziksel px): çöp kutusunun üstüne bırakıldıysa geri dönüşüm kutusuna
/// taşınır, değilse uygulama olarak sabitlenmeye çalışılır.
pub fn on_files_dropped(files: Vec<String>, x: i32) {
    let left = with(|w| w.ui.upgrade().filter(|ui| ui.get_w_trash()).map(|ui| ui.get_trash_left())).flatten();
    match left {
        Some(left) if x as f32 >= left * crate::dock::scale() => recycle::recycle(files),
        _ => {
            let (dirs, files): (Vec<String>, Vec<String>) =
                files.into_iter().partition(|f| std::path::Path::new(f).is_dir());
            if !dirs.is_empty() {
                add_stacks(dirs);
            }
            if !files.is_empty() {
                crate::apps::on_files_dropped(files);
            }
        }
    }
}

fn on_weather(data: weather::Weather) {
    with(|w| {
        w.sys.weather_has = true;
        w.sys.weather_icon = data.icon().into();
        w.sys.weather_temp = format!("{}°", data.temp_c.round() as i32).into();
        w.sys.weather_desc = data.description.into();
        w.push_sys();
    });
}

fn on_network(n: sysstatus::NetState) {
    with(|w| {
        w.sys.net_icon = net_icon(&n).into();
        w.push_sys();
    });
}

fn on_volume(v: sysstatus::VolumeState) {
    with(|w| {
        w.sys.volume_icon = volume_icon(&v).into();
        w.volume = v.level;
        crate::size_panel::show_value(crate::size_panel::Kind::Volume, v.level);
        w.push_sys();
    });
}

fn on_media(m: media::MediaState) {
    with(|w| {
        w.media = m;
        w.push_media();
        w.update_timers();
    });
}

fn on_art(art: Option<media::ArtPixels>) {
    with(|w| {
        w.art = art.map(|p| {
            let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&p.rgba, p.width, p.height);
            Image::from_rgba8(buf)
        });
        w.push_media();
    });
}

/// Ayarlardan widget'lar açılıp kapatıldı.
pub fn apply_config(c: WidgetsCfg) {
    with(|w| {
        if w.cfg == c {
            return;
        }
        let stats_on = c.stats && !w.cfg.stats;
        w.cfg = c;
        w.push_cfg();
        if stats_on && w.visible {
            w.sample_stats();
        }
        w.update_timers();
    });
}

/// Dock gizlenince/görününce: gizliyken CPU/RAM ve ilerleme güncellenmez.
pub fn set_visible(visible: bool) {
    with(|w| {
        if w.visible == visible {
            return;
        }
        w.visible = visible;
        if visible {
            w.sample_stats();
            w.push_media();
        }
        w.update_timers();
    });
}

/// WM_POWERBROADCAST.
pub fn on_power() {
    with(|w| w.update_battery());
}

pub fn on_clicked(id: i32) {
    match id {
        ID_MEDIA_PREV => media::previous(),
        ID_MEDIA_TOGGLE => media::play_pause(),
        ID_MEDIA_NEXT => media::next(),
        ID_TRAY => open_tray(),
        ID_QUICK => win::send_combo(&[win::KEY_WIN, win::KEY_A]),
        ID_CLOCK | ID_BELL => win::send_combo(&[win::KEY_WIN, win::KEY_N]),
        ID_TRASH => recycle::open(),
        id if id >= ID_STACK => open_stack((id - ID_STACK) as usize),
        _ => {}
    }
}

/// Fare tekerleği: ses alanında (ağ/ses/pil) bir çentik = %2. Seviye, ses
/// panelinde yüzdesiyle görünür.
pub fn on_scroll(id: i32, delta: f32) {
    if id != ID_QUICK {
        return;
    }
    let steps = crate::apps::wheel_steps(-1 - id, delta);
    let Some(level) = with(|w| {
        if steps != 0 {
            w.volume = (((w.volume + steps as f32 * 0.02) * 50.0).round() / 50.0).clamp(0.0, 1.0);
        }
        w.volume
    }) else {
        return;
    };
    if steps != 0 {
        sysstatus::set_volume(level);
    }
    crate::dock::open_volume_panel(level);
    crate::size_panel::show_value(crate::size_panel::Kind::Volume, level);
}
