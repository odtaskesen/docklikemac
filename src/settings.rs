//! Ayarlar penceresi (5g): normal, odak alan bir Slint penceresi (5a kararı).
//! Sadece açıkken var: kapanınca bileşen bırakılır. Değişiklikler canlı
//! uygulanır (`dock::update_config`), kayıt kısa gecikmeyle yapılır.

use std::cell::RefCell;
use std::time::Duration;

use slint::{CloseRequestResponse, ComponentHandle, Timer};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE};

use crate::config::{Appearance, Config, WidgetsCfg};
use crate::{appbar, dock, launch, SettingsWindow};

/// DWMWA_USE_IMMERSIVE_DARK_MODE: koyu başlık çubuğu.
const DWMWA_DARK: u32 = 20;

thread_local! {
    static WIN: RefCell<Option<SettingsWindow>> = const { RefCell::new(None) };
}

fn fill(w: &SettingsWindow, c: &Config) {
    w.set_zoom(c.scale);
    w.set_magnify(c.magnify);
    w.set_mag_max(c.magnify_max);
    w.set_appearance(c.appearance.index());
    w.set_auto_hide(c.auto_hide);
    let g = c.widgets;
    w.set_w_music(g.music);
    w.set_w_weather(g.weather);
    w.set_w_stats(g.stats);
    w.set_w_system(g.system);
    w.set_w_clock(g.clock);
    w.set_w_trash(g.trash);
}

fn read_widgets(w: &SettingsWindow) -> WidgetsCfg {
    WidgetsCfg {
        music: w.get_w_music(),
        weather: w.get_w_weather(),
        stats: w.get_w_stats(),
        system: w.get_w_system(),
        clock: w.get_w_clock(),
        trash: w.get_w_trash(),
    }
}

/// Ayarlar penceresini açar; zaten açıksa öne getirir.
pub fn open() {
    let existing = WIN.with(|s| s.borrow().as_ref().and_then(|w| crate::hwnd_of(w.window())));
    if let Some(h) = existing {
        // SAFETY: kendi penceremiz; menü seçiminden hemen sonra öne gelme izni var.
        unsafe {
            ShowWindow(h, SW_RESTORE);
            SetForegroundWindow(h);
        }
        return;
    }
    let Some(cfg) = dock::config() else { return };
    let w = match SettingsWindow::new() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[ayarlar] pencere açılamadı: {e}");
            return;
        }
    };
    fill(&w, &cfg);
    w.set_startup(dock::startup_enabled());
    let rgb = launch::accent_color();
    w.set_accent(slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));

    w.on_zoom_changed(|z| dock::update_config(|c| c.scale = z));
    let weak = w.as_weak();
    w.on_magnify_changed(move || {
        let Some(w) = weak.upgrade() else { return };
        dock::update_config(|c| {
            c.magnify = w.get_magnify();
            c.magnify_max = w.get_mag_max();
        });
    });
    w.on_appearance_changed(|i| dock::update_config(|c| c.appearance = Appearance::from_index(i.max(0) as u32)));
    w.on_auto_hide_changed(|on| dock::update_config(|c| c.auto_hide = on));
    w.on_startup_changed(|on| {
        dock::set_startup(on);
    });
    let weak = w.as_weak();
    w.on_widgets_changed(move || {
        let Some(w) = weak.upgrade() else { return };
        let g = read_widgets(&w);
        dock::update_config(|c| c.widgets = g);
    });
    let weak = w.as_weak();
    w.on_reset(move || {
        let Some(w) = weak.upgrade() else { return };
        let d = Config::default();
        dock::update_config(|c| {
            c.scale = d.scale;
            c.magnify = d.magnify;
            c.magnify_max = d.magnify_max;
            c.appearance = d.appearance;
            c.auto_hide = d.auto_hide;
            c.widgets = d.widgets;
        });
        if let Some(c) = dock::config() {
            fill(&w, &c);
        }
    });
    w.window().on_close_requested(|| {
        // Bileşen kendi geri çağrısının içinde bırakılmasın.
        Timer::single_shot(Duration::ZERO, close);
        CloseRequestResponse::HideWindow
    });

    // Ekran dışında açılsın, HWND gelince ana ekranın ortasına taşınsın.
    w.window().set_position(slint::PhysicalPosition::new(-32000, -32000));
    if let Err(e) = w.show() {
        eprintln!("[ayarlar] gösterilemedi: {e}");
        return;
    }
    let weak = w.as_weak();
    WIN.with(|s| *s.borrow_mut() = Some(w));
    place_when_ready(weak, 0);
}

fn place_when_ready(weak: slint::Weak<SettingsWindow>, tries: u32) {
    Timer::single_shot(Duration::from_millis(10), move || {
        let Some(w) = weak.upgrade() else { return };
        let Some(h) = crate::hwnd_of(w.window()) else {
            if tries < 50 {
                place_when_ready(w.as_weak(), tries + 1);
            }
            return;
        };
        let dark: i32 = 1;
        // SAFETY: geçerli HWND, 4 baytlık BOOL değeri.
        unsafe {
            DwmSetWindowAttribute(h, DWMWA_DARK, &dark as *const i32 as *const _, 4);
        }
        let m = appbar::primary_monitor_rect();
        let size = w.window().size();
        let x = m.left + (m.right - m.left - size.width as i32) / 2;
        let y = m.top + (m.bottom - m.top - size.height as i32) / 2;
        w.window().set_position(slint::PhysicalPosition::new(x, y));
        // SAFETY: kendi penceremiz.
        unsafe { SetForegroundWindow(h) };
    });
}

/// Ayarlar penceresini kapatır (açıksa) ve bekleyen kaydı yapar.
pub fn close() {
    let w = WIN.with(|s| s.borrow_mut().take());
    if let Some(w) = w {
        let _ = w.hide();
        drop(w);
    }
    dock::flush_save();
}
