//! Dock — Windows 11 için macOS tarzı dock.

// Release derlemesinde konsol penceresi açılmasın.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appbar;
mod apps;
mod config;
mod dock;
mod icons;
mod launch;
mod media;
mod popup;
mod preview;
mod procs;
mod recycle;
mod settings;
mod shell_thread;
mod size_panel;
mod stack_grid;
mod stacks;
mod sysstatus;
mod taskbar;
mod tracker;
mod weather;
mod widgets;
mod win;

use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{BOOL, HWND};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Threading::CreateMutexW;

slint::include_modules!();

/// AppBar kaydını kaldırır ve görev çubuğunu geri getirir. Her yoldan
/// (normal çıkış, panic, Ctrl+C, oturum kapanışı) çağrılır; tekrar çağrı güvenli.
pub fn cleanup() {
    appbar::unregister();
    taskbar::restore();
}

fn hwnd_of(w: &slint::Window) -> Option<HWND> {
    let handle = w.window_handle();
    let raw = HasWindowHandle::window_handle(&handle).ok()?.as_raw();
    match raw {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
        _ => None,
    }
}

unsafe extern "system" fn ctrl_handler(_ctrl: u32) -> BOOL {
    cleanup();
    0 // varsayılan işleyici süreci sonlandırsın
}

/// Winit penceresi olay döngüsü başladıktan sonra oluşur; HWND gelene kadar dene.
fn init_when_ready(weak: slint::Weak<DockWindow>) {
    slint::Timer::single_shot(Duration::from_millis(16), move || {
        let Some(ui) = weak.upgrade() else { return };
        match hwnd_of(ui.window()) {
            Some(h) => dock::init(&ui, h),
            None => init_when_ready(weak),
        }
    });
}

fn main() -> Result<(), slint::PlatformError> {
    if std::env::args().any(|a| a == "--restore") {
        taskbar::restore_from_file();
        println!("Görev çubuğu geri getirildi.");
        return Ok(());
    }

    // Tek kopya: muteks zaten varsa başka bir dock çalışıyor.
    let name: Vec<u16> = "Local\\DockLikeMac".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: name NUL sonlu; tutamaç süreç bitene dek açık kalır (bilerek kapatılmaz).
    let _mutex = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    // SAFETY: basit sorgu, CreateMutexW'den hemen sonra.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        eprintln!("Dock zaten çalışıyor.");
        return Ok(());
    }

    if std::env::var_os("SLINT_BACKEND").is_none() {
        // Saydam pencere için GPU çizici.
        std::env::set_var("SLINT_BACKEND", "winit-femtovg");
    }

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        cleanup();
        prev_hook(info);
    }));
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), 1) };

    let ui = DockWindow::new()?;
    ui.on_start_clicked(|| win::send_combo(&[win::KEY_WIN]));
    ui.on_context_menu(dock::on_context_menu);
    ui.on_app_clicked(apps::on_clicked);
    ui.on_app_menu(apps::on_menu);
    ui.on_app_reorder(apps::on_reorder);
    ui.on_app_drag_out(apps::on_drag_out);
    ui.on_app_new_window(apps::on_new_window);
    ui.on_app_scroll(apps::on_scroll);
    ui.on_app_hover(apps::on_hover);
    preview::set_on_hidden(dock::on_menu_closed);
    ui.on_widget_scroll(widgets::on_scroll);
    ui.on_body_changed(dock::on_body_changed);
    ui.on_widget_clicked(widgets::on_clicked);
    ui.on_widget_menu(widgets::on_menu);
    ui.on_mag_target(dock::on_mag_target);
    // Dock kapanırken (WM_CLOSE) Ayarlar açıksa o da kapansın, yoksa olay döngüsü sürer.
    ui.window().on_close_requested(|| {
        settings::close();
        slint::CloseRequestResponse::HideWindow
    });

    if !shell_thread::spawn() {
        eprintln!("[kısayol] Ctrl+Alt+D kaydedilemedi (başka bir uygulama kullanıyor olabilir)");
    }

    // Doğru yere taşınana kadar ekran dışında açılsın.
    ui.window().set_position(slint::PhysicalPosition::new(-32000, -32000));
    ui.show()?;
    init_when_ready(ui.as_weak());
    let result = slint::run_event_loop();
    settings::close();
    cleanup();
    result
}
