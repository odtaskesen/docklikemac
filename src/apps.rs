//! Dock'taki uygulamalar: sabitlenmiş + açık (sabitlenmemiş) uygulamalar,
//! tıklama davranışı, sağ tık menüsü, sabitleme/sıralama, ikon yükleme.
//!
//! Pencere listesi `tracker`dan olay geldikçe güncellenir. İkonlar ve görünen
//! adlar ayrı bir iş parçacığında yüklenir, gelince model yenilenir.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::config::PinnedApp;
use crate::shell_thread::{self, MenuItem};
use crate::tracker::{self, AppWindow, Snapshot};
use crate::{dock, icons, launch, procs, win, AppItem, DockWindow};

// Uygulama menüsü komutları.
const ID_NEW_WINDOW: u32 = 1;
const ID_TOGGLE_PIN: u32 = 2;
const ID_STARTUP: u32 = 3;
const ID_SHOW_IN_FOLDER: u32 = 4;
const ID_HIDE: u32 = 5;
const ID_CLOSE: u32 = 6;
const ID_FORCE_CLOSE: u32 = 7;
/// 1000 + k: k. pencereye geç.
const ID_WINDOW_BASE: u32 = 1000;

const APPS_FOLDER: &str = "shell:AppsFolder\\";
/// Dock görünürken arka plan süreçlerinin yoklanma aralığı.
const PROC_POLL: Duration = Duration::from_secs(4);

/// Açılış zıplaması: bir zıplamanın süresi, en uzun süre, büyümeyi bastırma geçişleri (sn).
const BOUNCE_PERIOD: f32 = 0.45;
const BOUNCE_MAX: f32 = 3.0;
const HOLD_IN: f32 = 0.12;
const HOLD_OUT: f32 = 0.15;
const BOUNCE_FRAME: Duration = Duration::from_millis(15);

/// Zıplayan bir uygulama.
struct Bounce {
    start: Instant,
    /// Penceresi geldi ya da süre doldu: zıplama bu anda (başlangıçtan sn) yere iner.
    end_at: Option<f32>,
}

/// Dock'ta görünen bir uygulama.
#[derive(Clone, Debug)]
struct App {
    name: String,
    exe: Option<String>,
    aumid: Option<String>,
    launch: String,
    args: String,
    pinned: bool,
    /// Z sırasına göre (en üstteki ilk).
    windows: Vec<AppWindow>,
    /// İkon önbelleği anahtarı = ikonun yükleneceği kaynak.
    icon_source: String,
    /// Bu uygulamanın pencerelerinde görülen, `exe`den farklı exe yolları (küçük harf).
    seen_exes: Vec<String>,
}

impl App {
    fn running(&self) -> bool {
        !self.windows.is_empty()
    }

    fn to_pinned(&self) -> PinnedApp {
        PinnedApp {
            name: self.name.clone(),
            exe: self.exe.clone(),
            aumid: self.aumid.clone(),
            launch: self.launch.clone(),
            args: self.args.clone(),
            icon: (self.icon_source != self.launch).then(|| self.icon_source.clone()),
            seen_exes: self.seen_exes.clone(),
        }
    }

    /// Arka planda çalışma tespitinde bakılacak exe yolları.
    fn exe_set(&self) -> impl Iterator<Item = &String> {
        self.exe.iter().chain(self.seen_exes.iter())
    }
}

/// `"C:\a b\x.exe" --arg` → (`C:\a b\x.exe`, `--arg`).
fn split_command(cmd: &str) -> (String, String) {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
    }
    // Tırnaksız yol boşluk içerebilir (Steam: `C:\Program Files (x86)\Steam\steam.exe`):
    // ".exe"den sonra böl.
    if let Some(i) = cmd.to_lowercase().find(".exe") {
        let end = i + ".exe".len();
        if cmd[end..].is_empty() || cmd[end..].starts_with(' ') {
            return (cmd[..end].to_string(), cmd[end..].trim().to_string());
        }
    }
    match cmd.split_once(' ') {
        Some((a, b)) => (a.to_string(), b.trim().to_string()),
        None => (cmd.to_string(), String::new()),
    }
}

/// İkon kaynağındaki ",0" / ",-123" gibi indeks ekini atar.
fn strip_icon_index(s: &str) -> String {
    match s.rsplit_once(',') {
        Some((path, idx)) if idx.trim().parse::<i32>().is_ok() => path.trim().to_string(),
        _ => s.trim().to_string(),
    }
}

/// Exe yolunun küçük harfli dosya adı.
fn base_name(exe: &str) -> String {
    exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_lowercase()
}

/// Kabuk süreci her zaman çalışır; tray tespitine katılmaz.
fn is_shell_exe(exe: &str) -> bool {
    base_name(exe) == "explorer.exe"
}

struct IconRequest {
    source: String,
    size: u32,
}

struct IconResult {
    source: String,
    pixels: Option<icons::IconPixels>,
    name: String,
}

struct Apps {
    pinned: Vec<PinnedApp>,
    snapshot: Snapshot,
    /// Sabitlenmemiş açık uygulamaların ilk görülme sırası (anahtarları).
    unpinned_order: Vec<String>,
    list: Vec<App>,
    icons: HashMap<String, Image>,
    names: HashMap<String, String>,
    requested: HashSet<String>,
    model: Rc<VecModel<AppItem>>,
    icon_tx: mpsc::Sender<IconRequest>,
    icon_size: u32,
    /// Sabitlerin exe adlarıyla eşleşen çalışan süreçlerin yolları.
    running_exes: HashSet<String>,
    procs_tx: mpsc::Sender<ProcRequest>,
    /// Başlatma hedefi süreçten kontrol edilmiş sabit exe'leri (bir kez).
    launch_checked: HashSet<String>,
    _procs_timer: slint::Timer,
    /// Zıplayan uygulamalar (anahtar: `bounce_key`). Zamanlayıcı sadece zıplarken çalışır.
    bouncing: HashMap<String, Bounce>,
    bounce_timer: slint::Timer,
}

fn bounce_key(a: &App) -> String {
    format!("{}|{}", a.launch, a.args).to_lowercase()
}

thread_local! {
    static STATE: RefCell<Option<Apps>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Apps) -> R) -> Option<R> {
    STATE.with(|s| {
        let mut s = s.try_borrow_mut().ok()?;
        s.as_mut().map(f)
    })
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// Paketli (Store/UWP) uygulama kimlikleri "PaketAilesi!UygulamaKimliği" biçimindedir.
fn is_packaged(aumid: &Option<String>) -> bool {
    aumid.as_deref().is_some_and(|a| a.contains('!'))
}

/// Pencerenin anahtarı: AppUserModelID varsa o, yoksa exe yolu.
fn window_key(w: &AppWindow) -> String {
    w.aumid.clone().unwrap_or_else(|| w.exe.clone())
}

/// İkonun ve başlatmanın kaynağı.
fn window_source(w: &AppWindow) -> String {
    if is_packaged(&w.aumid) {
        format!("{APPS_FOLDER}{}", w.aumid.as_deref().unwrap_or_default())
    } else {
        w.launch.clone().unwrap_or_else(|| w.exe.clone())
    }
}

/// Pencere bu sabitlenmiş uygulamaya mı ait?
/// İkisinde de AppUserModelID varsa o karşılaştırılır, yoksa exe yolu.
fn matches(p: &PinnedApp, w: &AppWindow) -> bool {
    match (&p.aumid, &w.aumid) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => p.exe.as_deref().is_some_and(|e| exe_key(e) == exe_key(&w.exe)),
    }
}

/// Exe karşılaştırma anahtarı: küçük harf; Store uygulamalarında sürüm ve mimari
/// atılır (uygulama güncellenince klasör adı değişiyor).
fn exe_key(exe: &str) -> String {
    match launch::windowsapps_parts(exe) {
        Some((name, publisher, tail)) => format!("windowsapps:{name}_{publisher}\\{tail}"),
        None => exe.to_lowercase(),
    }
}

/// Kısayol adındaki " - Kısayol" / " - Shortcut" ve ".exe" eklerini temizler.
fn clean_name(name: &str) -> String {
    let mut n = name.trim().to_string();
    for suffix in [" - Kısayol", " - Shortcut", " - kısayol", " - shortcut"] {
        if let Some(s) = n.strip_suffix(suffix) {
            n = s.to_string();
        }
    }
    if n.to_lowercase().ends_with(".exe") {
        n.truncate(n.len() - 4);
    }
    n
}

/// Bir .lnk ya da .exe dosyasından sabit oluşturur.
fn pinned_from_file(path: &str) -> Option<PinnedApp> {
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "lnk" => {
            let info = launch::resolve_link(path)?;
            let mut exe = info
                .target
                .filter(|t| t.to_lowercase().ends_with(".exe"))
                .map(|t| lower(&t));
            // Dosya Gezgini kısayolunun hedefi yok; pencereleri explorer.exe'dir.
            if exe.is_none()
                && info.aumid.as_deref() == Some("Microsoft.Windows.Explorer")
            {
                exe = Some(r"c:\windows\explorer.exe".into());
            }
            Some(PinnedApp {
                name: clean_name(&launch::display_name(path)),
                exe,
                aumid: info.aumid,
                launch: path.to_string(),
                args: String::new(),
                icon: None,
                seen_exes: Vec::new(),
            })
        }
        "exe" => Some(PinnedApp {
            name: clean_name(&launch::display_name(path)),
            exe: Some(lower(path)),
            aumid: None,
            launch: path.to_string(),
            args: String::new(),
            icon: None,
            seen_exes: Vec::new(),
        }),
        _ => None,
    }
}

fn to_image(p: &icons::IconPixels) -> Image {
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&p.rgba, p.width, p.height);
    Image::from_rgba8(buf)
}

/// Ekran ölçeğine göre ikon boyutu (büyümüş ikonun boyutuna yakın, standart boylar).
pub fn icon_size_for(scale: f32) -> u32 {
    // En büyük hücre: 44 px × en büyük büyüme oranı × en büyük dock boyutu; ikon hücrenin %80'i.
    let need = (44.0 * crate::config::MAG_MAX * 0.8 * crate::size_panel::MAX * scale).ceil() as u32;
    [48, 64, 96, 128, 256].into_iter().find(|&s| s >= need).unwrap_or(256)
}

/// Süreç yoklama iş parçacığı: istenen exe adlarıyla çalışan süreçleri bulur.
/// Toolhelp anlık görüntüsü birkaç ms sürdüğü için UI iş parçacığında yapılmaz.
struct ProcRequest {
    /// Aranacak küçük harfli exe adları.
    names: HashSet<String>,
    /// Çalışıyorsa sürecinden doğru başlatma hedefi bulunacak sabit exe yolları.
    fix: Vec<String>,
}

fn spawn_procs_worker() -> mpsc::Sender<ProcRequest> {
    let (tx, rx) = mpsc::channel::<ProcRequest>();
    std::thread::Builder::new()
        .name("procs".into())
        .spawn(move || {
            while let Ok(mut req) = rx.recv() {
                // Biriken isteklerden sadece sonuncusu (düzeltilecekler birikir).
                while let Ok(n) = rx.try_recv() {
                    let fix = std::mem::take(&mut req.fix);
                    req = n;
                    req.fix.extend(fix);
                }
                let running = if req.names.is_empty() {
                    HashMap::new()
                } else {
                    procs::running_exes_matching(&req.names)
                };
                let fixes: Vec<(String, String)> = req
                    .fix
                    .iter()
                    .filter_map(|e| {
                        let pid = *running.get(e)?;
                        Some((e.clone(), tracker::find_launch(pid, e)?))
                    })
                    .collect();
                let found: HashSet<String> = running.keys().map(|e| exe_key(e)).collect();
                let _ = slint::invoke_from_event_loop(move || on_procs(found, fixes));
            }
        })
        .ok();
    tx
}

fn on_procs(found: HashSet<String>, fixes: Vec<(String, String)>) {
    with(|s| {
        let mut changed = false;
        for (exe, target) in fixes {
            for p in &mut s.pinned {
                if p.launch.eq_ignore_ascii_case(&exe) && p.args.is_empty() {
                    p.launch = target.clone();
                    let n = clean_name(&launch::display_name(&target));
                    if !n.is_empty() {
                        p.name = n;
                    }
                    changed = true;
                }
            }
        }
        if changed {
            s.save();
        }
        if s.running_exes != found {
            s.running_exes = found;
            s.push_model();
        }
    });
}

/// Arka plan (tray) durumunu tazeler; dock yeniden görününce de çağrılır.
pub fn refresh_procs() {
    with(|s| s.request_procs());
}

fn spawn_icon_worker() -> mpsc::Sender<IconRequest> {
    let (tx, rx) = mpsc::channel::<IconRequest>();
    std::thread::Builder::new()
        .name("icon-loader".into())
        .spawn(move || {
            icons::com_init_thread();
            for req in rx {
                let pixels = icons::load_icon(&req.source, req.size);
                let name = clean_name(&launch::display_name(&req.source));
                let result = IconResult { source: req.source, pixels, name };
                let _ = slint::invoke_from_event_loop(move || on_icon_loaded(result));
            }
        })
        .ok();
    tx
}

impl Apps {
    /// Sabitler + pencere listesinden dock listesini yeniden kurar.
    fn rebuild(&mut self) {
        let wins = &self.snapshot.windows;
        let mut used = vec![false; wins.len()];
        let mut list = Vec::new();

        let mut pins_changed = false;
        for p in &mut self.pinned {
            let mut ws = Vec::new();
            for (i, w) in wins.iter().enumerate() {
                if !used[i] && matches(p, w) {
                    used[i] = true;
                    ws.push(w.clone());
                }
            }
            // Pencerelerden öğrenilenleri sabite kaydet: asıl exe, profil ikonu.
            for w in &ws {
                // Pencereden sabitlenmiş eski kayıtlar başlatma hedefi olarak pencerenin
                // exe'sini tutuyor; bu, Store uygulamalarında ve yardımcı süreçlerde
                // (steamwebhelper) uygulamayı açmaz. Doğru hedefe bir kez düzelt.
                if let Some(t) = &w.launch {
                    if p.launch.eq_ignore_ascii_case(&w.exe) && p.args.is_empty() {
                        p.launch = t.clone();
                        let n = clean_name(&launch::display_name(t));
                        if !n.is_empty() {
                            p.name = n;
                        }
                        pins_changed = true;
                    }
                }
                let k = exe_key(&w.exe);
                if p.exe.as_deref().map(exe_key) != Some(k.clone())
                    && !p.seen_exes.iter().any(|e| exe_key(e) == k)
                {
                    p.seen_exes.push(w.exe.clone());
                    pins_changed = true;
                }
                if p.icon.is_none() {
                    if let Some(ic) = &w.relaunch_icon {
                        p.icon = Some(strip_icon_index(ic));
                        pins_changed = true;
                    }
                }
            }
            list.push(App {
                name: p.name.clone(),
                exe: p.exe.clone(),
                aumid: p.aumid.clone(),
                launch: p.launch.clone(),
                args: p.args.clone(),
                pinned: true,
                windows: ws,
                icon_source: p.icon.clone().unwrap_or_else(|| p.launch.clone()),
                seen_exes: p.seen_exes.clone(),
            });
        }
        if pins_changed {
            dock::save_pinned(self.pinned.clone());
        }

        // Kalan pencereleri uygulamaya göre grupla.
        let mut groups: HashMap<String, Vec<AppWindow>> = HashMap::new();
        let mut first_seen = Vec::new();
        for (i, w) in wins.iter().enumerate() {
            if used[i] {
                continue;
            }
            let key = window_key(w);
            if !groups.contains_key(&key) {
                first_seen.push(key.clone());
            }
            groups.entry(key).or_default().push(w.clone());
        }
        self.unpinned_order.retain(|k| groups.contains_key(k));
        for k in first_seen {
            if !self.unpinned_order.contains(&k) {
                self.unpinned_order.push(k);
            }
        }
        for key in &self.unpinned_order {
            let Some(ws) = groups.remove(key) else { continue };
            let w0 = &ws[0];
            let source = window_source(w0);
            // Profilli uygulamalar (Chrome, Edge): profilin ikonu, adı ve komutu.
            let (launch_target, args) = match &w0.relaunch_command {
                Some(cmd) => split_command(cmd),
                None => (source.clone(), String::new()),
            };
            let icon_source =
                w0.relaunch_icon.as_deref().map(strip_icon_index).unwrap_or_else(|| source.clone());
            let name = w0
                .relaunch_name
                .clone()
                .or_else(|| self.names.get(&source).cloned())
                .unwrap_or_else(|| {
                    clean_name(
                        &std::path::Path::new(&w0.exe)
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    )
                });
            let mut seen_exes: Vec<String> = Vec::new();
            for w in &ws {
                if w.exe != w0.exe && !seen_exes.contains(&w.exe) {
                    seen_exes.push(w.exe.clone());
                }
            }
            list.push(App {
                name,
                exe: Some(w0.exe.clone()),
                aumid: w0.aumid.clone(),
                launch: launch_target,
                args,
                pinned: false,
                windows: ws,
                icon_source,
                seen_exes,
            });
        }

        self.list = list;
        self.request_icons();
        self.push_model();
    }

    fn request_icons(&mut self) {
        for a in &self.list {
            if self.requested.insert(a.icon_source.clone()) {
                let _ = self.icon_tx.send(IconRequest {
                    source: a.icon_source.clone(),
                    size: self.icon_size,
                });
            }
        }
    }

    /// Sabit uygulamanın penceresi yok ama süreci çalışıyor mu (tray'de).
    /// Exe'si başka bir öğenin penceresine aitse (ör. başka Chrome profili) sayılmaz.
    /// Paketli (Store) uygulamalar askıya alınmış süreçler bıraktığı için hariç.
    fn in_background(&self, a: &App, window_exes: &HashSet<String>) -> bool {
        a.pinned
            && !a.running()
            && !is_packaged(&a.aumid)
            && a.exe_set().any(|e| {
                let k = exe_key(e);
                !is_shell_exe(e) && self.running_exes.contains(&k) && !window_exes.contains(&k)
            })
    }

    /// Süreç yoklayıcısına sabitlerin exe adlarını gönderir.
    fn request_procs(&mut self) {
        let names: HashSet<String> = self
            .list
            .iter()
            .filter(|a| a.pinned && !is_packaged(&a.aumid))
            .flat_map(|a| a.exe_set())
            .filter(|e| !is_shell_exe(e))
            .map(|e| base_name(e))
            .collect();
        // Pencereden sabitlenmiş eski kayıtlar (başlatma hedefi = exe): penceresi
        // yokken de süreçten düzeltilebilsin.
        let fix: Vec<String> = self
            .pinned
            .iter()
            .filter(|p| p.args.is_empty() && p.exe.as_deref().is_some_and(|e| p.launch.eq_ignore_ascii_case(e)))
            .filter_map(|p| p.exe.clone())
            .filter(|e| self.launch_checked.insert(e.clone()))
            .collect();
        let _ = self.procs_tx.send(ProcRequest { names, fix });
    }

    fn push_model(&self) {
        let fg = self.snapshot.foreground;
        let window_exes: HashSet<String> = self.snapshot.windows.iter().map(|w| exe_key(&w.exe)).collect();
        let bounce: HashMap<&String, (f32, f32)> =
            self.bouncing.iter().map(|(k, b)| (k, bounce_values(b))).collect();
        let items: Vec<AppItem> = self
            .list
            .iter()
            .map(|a| AppItem {
                icon: self.icons.get(&a.icon_source).cloned().unwrap_or_default(),
                running: a.running(),
                active: fg != 0 && a.windows.iter().any(|w| w.hwnd == fg),
                pinned: a.pinned,
                background: self.in_background(a, &window_exes),
                bounce: bounce.get(&bounce_key(a)).map_or(0.0, |v| v.0),
                hold: bounce.get(&bounce_key(a)).map_or(0.0, |v| v.1),
            })
            .collect();
        // Değişmeyen satırlara dokunma (gereksiz yeniden çizim olmasın).
        if self.model.row_count() == items.len() {
            for (i, it) in items.into_iter().enumerate() {
                if self.model.row_data(i).as_ref() != Some(&it) {
                    self.model.set_row_data(i, it);
                }
            }
        } else {
            self.model.set_vec(items);
        }
    }

    /// Kapalı uygulama başlatıldı: penceresi gelene kadar (en fazla ~3 sn) zıplat.
    fn start_bounce(&mut self, app: &App) {
        self.bouncing.insert(bounce_key(app), Bounce { start: Instant::now(), end_at: None });
        if !self.bounce_timer.running() {
            self.bounce_timer.start(slint::TimerMode::Repeated, BOUNCE_FRAME, || {
                with(|s| s.bounce_tick());
            });
        }
    }

    fn bounce_tick(&mut self) {
        let list = &self.list;
        self.bouncing.retain(|k, b| {
            let t = b.start.elapsed().as_secs_f32();
            if b.end_at.is_none() {
                let app = list.iter().find(|a| bounce_key(a) == *k);
                if t >= BOUNCE_MAX || app.is_none_or(|a| a.running()) {
                    // Havadaysa yere inmesini bekle.
                    b.end_at = Some((t / BOUNCE_PERIOD).ceil() * BOUNCE_PERIOD);
                }
            }
            b.end_at.is_none_or(|e| t < e + HOLD_OUT)
        });
        if self.bouncing.is_empty() {
            self.bounce_timer.stop();
        }
        self.push_model();
    }

    fn save(&mut self) {
        dock::save_pinned(self.pinned.clone());
        self.rebuild();
    }

    fn pin(&mut self, app: &App, at: Option<usize>) {
        if self.pinned.iter().any(|p| p.launch.eq_ignore_ascii_case(&app.launch)) {
            return;
        }
        let at = at.unwrap_or(self.pinned.len()).min(self.pinned.len());
        self.pinned.insert(at, app.to_pinned());
        self.save();
    }

    fn unpin(&mut self, app: &App) {
        self.pinned.retain(|p| !p.launch.eq_ignore_ascii_case(&app.launch));
        self.save();
    }
}

/// (yükseklik 0..1, büyümeyi bastırma 0..1). Zıplama yerçekimi gibi: parabol.
fn bounce_values(b: &Bounce) -> (f32, f32) {
    let t = b.start.elapsed().as_secs_f32();
    let end = b.end_at.unwrap_or(f32::MAX);
    let hold = if t < end { (t / HOLD_IN).min(1.0) } else { (1.0 - (t - end) / HOLD_OUT).max(0.0) };
    let lift = if t < end {
        let p = (t % BOUNCE_PERIOD) / BOUNCE_PERIOD;
        4.0 * p * (1.0 - p)
    } else {
        0.0
    };
    (lift, hold)
}

/// Uygulamayı başlatır; kapalıysa zıplatır.
fn launch_app(app: &App) {
    launch::launch(&app.launch, &app.args);
    if !app.running() {
        with(|s| s.start_bounce(app));
    }
}

fn startup_name(app: &App) -> String {
    app.name.clone()
}

fn startup_command(app: &App) -> String {
    if app.launch.to_lowercase().starts_with("shell:") {
        format!("explorer.exe {}", app.launch)
    } else if app.args.is_empty() {
        format!("\"{}\"", app.launch)
    } else {
        format!("\"{}\" {}", app.launch, app.args)
    }
}

/// Menü metnindeki & işaretleri kısayol tuşu sanılmasın.
fn menu_text(s: &str) -> String {
    let mut t: String = s.chars().take(60).collect();
    if s.chars().count() > 60 {
        t.push('…');
    }
    t.replace('&', "&&")
}

fn app_menu(app: &App, shift: bool) -> Vec<MenuItem> {
    let mut items = Vec::new();
    // Profilli uygulamalarda (Chrome, Edge) hangi profil olduğu başta görünsün.
    if app.args.contains("--profile-directory") {
        items.push(MenuItem::Item { id: 0, text: menu_text(&app.name), checked: false, enabled: false });
        items.push(MenuItem::Separator);
    }
    for (k, w) in app.windows.iter().enumerate() {
        let title = if w.title.trim().is_empty() { &app.name } else { &w.title };
        items.push(MenuItem::item(ID_WINDOW_BASE + k as u32, menu_text(title)));
    }
    if !items.is_empty() {
        items.push(MenuItem::Separator);
    }
    items.push(MenuItem::item(ID_NEW_WINDOW, "Yeni Pencere"));
    items.push(MenuItem::sub(
        "Seçenekler",
        vec![
            MenuItem::item(ID_TOGGLE_PIN, if app.pinned { "Dock'tan Kaldır" } else { "Dock'ta Tut" }),
            MenuItem::check(
                ID_STARTUP,
                "Oturum Açılışında Aç",
                launch::startup_enabled(&startup_name(app)),
            ),
            MenuItem::item(ID_SHOW_IN_FOLDER, "Dosya Konumunda Göster"),
        ],
    ));
    if app.running() {
        items.push(MenuItem::Separator);
        items.push(MenuItem::item(ID_HIDE, "Gizle"));
        items.push(MenuItem::item(ID_CLOSE, "Kapat"));
        if shift {
            items.push(MenuItem::item(ID_FORCE_CLOSE, "Zorla Kapat"));
        }
    }
    items
}

fn run_menu_command(app: App, cmd: u32) {
    match cmd {
        0 => {}
        ID_NEW_WINDOW => launch_app(&app),
        ID_TOGGLE_PIN => {
            with(|s| if app.pinned { s.unpin(&app) } else { s.pin(&app, None) });
        }
        ID_STARTUP => {
            let name = startup_name(&app);
            let on = !launch::startup_enabled(&name);
            launch::set_startup(&name, &startup_command(&app), on);
        }
        ID_SHOW_IN_FOLDER => {
            let path = app.exe.clone().filter(|e| !e.is_empty()).unwrap_or(app.launch.clone());
            launch::show_in_folder(&path);
        }
        ID_HIDE => app.windows.iter().for_each(|w| tracker::minimize(w.hwnd)),
        ID_CLOSE => app.windows.iter().for_each(|w| tracker::close(w.hwnd)),
        ID_FORCE_CLOSE => {
            let pids: HashSet<u32> = app.windows.iter().map(|w| w.pid).collect();
            for pid in pids {
                tracker::force_kill(pid);
            }
        }
        c if c >= ID_WINDOW_BASE => {
            if let Some(w) = app.windows.get((c - ID_WINDOW_BASE) as usize) {
                tracker::activate(w.hwnd);
            }
        }
        _ => {}
    }
}

// ---- Dışarıdan çağrılanlar (UI iş parçacığı) ----

pub fn init(ui: &DockWindow, mut pinned: Vec<PinnedApp>, pins_imported: bool) {
    icons::com_init_thread();
    let model = Rc::new(VecModel::<AppItem>::default());
    ui.set_apps(model.clone().into());
    let rgb = launch::accent_color();
    ui.set_accent(slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));

    // İlk açılış: Windows görev çubuğu sabitlerini bir kez aktar.
    let mut import = !pins_imported;
    if import {
        for lnk in launch::taskbar_pins() {
            if let Some(p) = pinned_from_file(&lnk) {
                pinned.push(p);
            }
        }
    }
    // Store uygulamaları sürüm numaralı klasördeki exe'den başlatılamaz ve
    // güncellenince o klasör silinir: sürümden bağımsız AUMID ile başlat.
    for p in &mut pinned {
        // Eski hata: tırnaksız komut boşlukta bölünmüştü ("C:\Program" + "Files (x86)\...").
        if !p.args.is_empty()
            && !p.launch.to_lowercase().starts_with("shell:")
            && !std::path::Path::new(&p.launch).exists()
        {
            let (t, a) = split_command(&format!("{} {}", p.launch, p.args));
            if std::path::Path::new(&t).exists() {
                p.launch = t;
                p.args = a;
                import = true; // kaydet
            }
        }
        if launch::windowsapps_parts(&p.launch).is_some() {
            if let Some(a) = launch::packaged_aumid(&p.launch) {
                p.launch = format!("{APPS_FOLDER}{a}");
                import = true; // kaydet
            }
        }
    }

    let apps = Apps {
        pinned,
        snapshot: Snapshot::default(),
        unpinned_order: Vec::new(),
        list: Vec::new(),
        icons: HashMap::new(),
        names: HashMap::new(),
        requested: HashSet::new(),
        model,
        icon_tx: spawn_icon_worker(),
        icon_size: icon_size_for(dock::scale()),
        running_exes: HashSet::new(),
        procs_tx: spawn_procs_worker(),
        launch_checked: HashSet::new(),
        _procs_timer: {
            // Penceresiz süreç kapanınca pencere olayı gelmez; dock görünürken seyrek yokla.
            let t = slint::Timer::default();
            t.start(slint::TimerMode::Repeated, PROC_POLL, || {
                if dock::shown() {
                    refresh_procs();
                }
            });
            t
        },
        bouncing: HashMap::new(),
        bounce_timer: slint::Timer::default(),
    };
    STATE.with(|s| *s.borrow_mut() = Some(apps));
    with(|s| {
        if import {
            s.save();
        } else {
            s.rebuild();
        }
    });

    tracker::start(|snap| {
        let _ = slint::invoke_from_event_loop(move || on_snapshot(snap));
    });
}

fn on_snapshot(mut snap: Snapshot) {
    with(|s| {
        // Menü açıkken ön plan bizim yardımcı penceremize geçer; öndeki uygulamayı koru.
        if snap.foreground == 0 && shell_thread::menu_open() {
            snap.foreground = s.snapshot.foreground;
        }
        s.snapshot = snap;
        s.rebuild();
        s.request_procs();
    });
    refresh_preview();
}

fn on_icon_loaded(r: IconResult) {
    with(|s| {
        if let Some(p) = &r.pixels {
            s.icons.insert(r.source.clone(), to_image(p));
        }
        if !r.name.is_empty() {
            s.names.insert(r.source.clone(), r.name);
        }
        s.rebuild();
    });
}

pub fn on_clicked(index: i32) {
    suppress_preview(index);
    let Some((app, fg)) = with(|s| {
        (s.list.get(index as usize).cloned(), s.snapshot.foreground)
    }) else {
        return;
    };
    let Some(app) = app else { return };

    if !app.running() {
        launch_app(&app);
        return;
    }
    if app.windows.len() == 1 {
        let w = &app.windows[0];
        if w.hwnd == fg && !w.minimized {
            tracker::minimize(w.hwnd);
        } else {
            tracker::activate(w.hwnd);
        }
        return;
    }
    // Birden fazla pencere: öndeyse sıradakine geç, değilse en üsttekini getir.
    let mut stable: Vec<&AppWindow> = app.windows.iter().collect();
    stable.sort_by_key(|w| w.hwnd);
    match stable.iter().position(|w| w.hwnd == fg) {
        Some(i) => tracker::activate(stable[(i + 1) % stable.len()].hwnd),
        None => tracker::activate(app.windows[0].hwnd),
    }
}

/// Orta tık / Shift+tık: uygulamanın yeni penceresini açar.
pub fn on_new_window(index: i32) {
    suppress_preview(index);
    let Some(Some(app)) = with(|s| s.list.get(index as usize).cloned()) else {
        return;
    };
    launch_app(&app);
}

thread_local! {
    /// Tekerlek birikimi (dokunmatik yüzeyin küçük adımları için): (uygulama, toplam).
    static WHEEL: RefCell<(i32, f32)> = const { RefCell::new((-1, 0.0)) };
}

/// Tekerlek bir çentiği (60 px) doldurdukça adım sayısını döndürür (yukarı = pozitif).
pub fn wheel_steps(key: i32, delta: f32) -> i32 {
    WHEEL.with(|w| {
        let mut w = w.borrow_mut();
        if w.0 != key || w.1.signum() * delta.signum() < 0.0 {
            *w = (key, 0.0);
        }
        w.1 += delta;
        let steps = (w.1 / 60.0).trunc();
        w.1 -= steps * 60.0;
        steps as i32
    })
}

/// Fare tekerleği uygulama ikonunda: pencereleri arasında geçer
/// (aşağı = sıradaki, yukarı = önceki). Tek penceresi varsa öne getirir.
pub fn on_scroll(index: i32, delta: f32) {
    let steps = wheel_steps(index, delta);
    if steps == 0 {
        return;
    }
    let Some((Some(app), fg)) = with(|s| (s.list.get(index as usize).cloned(), s.snapshot.foreground))
    else {
        return;
    };
    if app.windows.is_empty() {
        return;
    }
    let mut stable: Vec<&AppWindow> = app.windows.iter().collect();
    stable.sort_by_key(|w| w.hwnd);
    let n = stable.len() as i32;
    match stable.iter().position(|w| w.hwnd == fg) {
        // Öndeki pencereden aşağı = ileri. Tek pencerede kendisi (öne getirir).
        Some(i) => tracker::activate(stable[(i as i32 - steps).rem_euclid(n) as usize].hwnd),
        // Uygulama önde değil: en üstteki penceresini getir.
        None => tracker::activate(app.windows[0].hwnd),
    }
}

pub fn on_menu(index: i32) {
    let Some(Some(app)) = with(|s| s.list.get(index as usize).cloned()) else {
        return;
    };
    suppress_preview(index);
    dock::hold_for_menu();
    let items = app_menu(&app, win::shift_down());
    let (x, y) = win::cursor_pos();
    shell_thread::show_menu(items, x, y, move |cmd| run_menu_command(app, cmd));
}

/// Sürükleyerek sıralama. Sabitlenmemiş bir uygulama sabitlerin arasına
/// bırakılırsa sabitlenir.
pub fn on_reorder(from: i32, to: i32) {
    with(|s| {
        let (from, to) = (from as usize, to as usize);
        let Some(app) = s.list.get(from).cloned() else { return };
        let n = s.pinned.len();
        if app.pinned {
            let Some(pi) = s.pinned.iter().position(|p| p.launch.eq_ignore_ascii_case(&app.launch))
            else {
                return;
            };
            let p = s.pinned.remove(pi);
            let to = to.min(s.pinned.len());
            s.pinned.insert(to, p);
            s.save();
        } else if to < n {
            s.pin(&app, Some(to));
        }
    });
}

/// Dock dışına sürüklenip bırakıldı: sabitse kaldır.
pub fn on_drag_out(index: i32) {
    with(|s| {
        if let Some(app) = s.list.get(index as usize).cloned() {
            if app.pinned {
                s.unpin(&app);
            }
        }
    });
}

/// Explorer'dan bırakılan .exe / .lnk dosyalarını sabitler.
pub fn on_files_dropped(files: Vec<String>) {
    with(|s| {
        let mut changed = false;
        for f in files {
            if let Some(p) = pinned_from_file(&f) {
                if !s.pinned.iter().any(|q| q.launch.eq_ignore_ascii_case(&p.launch)) {
                    s.pinned.push(p);
                    changed = true;
                }
            }
        }
        if changed {
            s.save();
        }
    });
}

// ---- Pencere önizlemesi (5e) ----
//
// İkonun üzerinde `PREVIEW_DELAY` durunca açık pencerelerin önizlemesi açılır.
// Açıkken başka bir ikona geçince beklemeden o uygulamanınkine geçer. İmleç ne
// bir ikonda ne de önizlemede ise `PREVIEW_LEAVE` sonra kapanır (ikonla önizleme
// arasındaki boşluktan geçerken kapanmasın diye). Tıklama, menü ve sürükleme
// önizlemeyi kapatır; aynı ikondan çıkıp girene kadar yeniden açılmaz.

const PREVIEW_DELAY: Duration = Duration::from_millis(450);
const PREVIEW_LEAVE: Duration = Duration::from_millis(300);
const PREVIEW_POLL: Duration = Duration::from_millis(50);

struct Hover {
    /// Üzerinde durulan uygulama (-1: yok) ve ikonun merkezi (mantıksal, pencereye göre).
    idx: i32,
    center: f32,
    /// Tıklanan uygulama: imleç başka bir ikona geçene ya da dock'tan çıkana kadar önizleme açılmaz.
    suppress: i32,
    /// Önizlemesi açık uygulamanın kimliği (`icon_source`); liste değişince yeniden bulmak için.
    shown: Option<String>,
    open_timer: slint::Timer,
    leave_timer: slint::Timer,
    left_at: Option<Instant>,
}

thread_local! {
    static HOVER: RefCell<Hover> = RefCell::new(Hover {
        idx: -1,
        center: 0.0,
        suppress: -1,
        shown: None,
        open_timer: slint::Timer::default(),
        leave_timer: slint::Timer::default(),
        left_at: None,
    });
}

fn with_hover<R>(f: impl FnOnce(&mut Hover) -> R) -> Option<R> {
    HOVER.with(|h| h.try_borrow_mut().ok().map(|mut h| f(&mut h)))
}

/// `app-hover`: imleç bir uygulama ikonuna girdi ya da ikonlardan çıktı.
pub fn on_hover(idx: i32, center: f32) {
    let open = crate::preview::is_open();
    let action = with_hover(|h| {
        h.idx = idx;
        h.center = center;
        h.open_timer.stop();
        if idx >= 0 && idx != h.suppress {
            h.suppress = -1;
        }
        if idx < 0 || idx == h.suppress {
            if open {
                start_leave_check(h);
            }
            return false;
        }
        if open {
            return true;
        }
        h.open_timer.start(slint::TimerMode::SingleShot, PREVIEW_DELAY, || {
            if let Some(i) = with_hover(|h| h.idx) {
                if i >= 0 {
                    show_preview(i);
                }
            }
        });
        false
    });
    if action == Some(true) {
        show_preview(idx);
    }
}

fn start_leave_check(h: &mut Hover) {
    h.left_at = None;
    if h.leave_timer.running() {
        return;
    }
    h.leave_timer.start(slint::TimerMode::Repeated, PREVIEW_POLL, || {
        let inside = crate::preview::cursor_inside();
        let hide = with_hover(|h| {
            if !crate::preview::is_open() {
                h.leave_timer.stop();
                return false;
            }
            if inside || (h.idx >= 0 && h.idx != h.suppress) {
                h.left_at = None;
                return false;
            }
            h.left_at.get_or_insert_with(Instant::now).elapsed() >= PREVIEW_LEAVE
        });
        if hide == Some(true) {
            hide_preview();
        }
    });
}

/// Uygulamanın pencerelerini önizlemede gösterir; penceresi yoksa önizlemeyi kapatır.
fn show_preview(idx: i32) {
    let Some(Some(app)) = with(|s| s.list.get(idx as usize).cloned()) else {
        return;
    };
    if app.windows.is_empty() {
        hide_preview();
        return;
    }
    let Some(center) = with_hover(|h| {
        h.shown = Some(app.icon_source.clone());
        h.center
    }) else {
        return;
    };
    let Some((cx, bottom, dpi)) = dock::popup_anchor(center) else {
        return;
    };
    // Kararlı sıra (hwnd'ye göre), tekerlekle geçişteki sırayla aynı.
    let mut ws: Vec<&AppWindow> = app.windows.iter().collect();
    ws.sort_by_key(|w| w.hwnd);
    let items = ws
        .into_iter()
        .map(|w| crate::preview::Item {
            hwnd: w.hwnd,
            title: if w.title.is_empty() { app.name.clone() } else { w.title.clone() },
            minimized: w.minimized,
        })
        .collect();
    crate::preview::show(
        items,
        cx,
        bottom,
        dpi,
        launch::accent_color(),
        tracker::activate,
        tracker::close,
    );
    with_hover(start_leave_check);
}

/// Önizlemeyi kapatır (açık değilse bir şey yapmaz).
pub fn hide_preview() {
    with_hover(|h| {
        h.open_timer.stop();
        h.leave_timer.stop();
        h.shown = None;
    });
    if crate::preview::is_open() {
        crate::preview::hide();
    }
}

/// Tıklama/menü: önizlemeyi kapat, imleç bu ikondan ayrılana kadar yeniden açma.
fn suppress_preview(idx: i32) {
    with_hover(|h| h.suppress = idx);
    hide_preview();
}

/// İmleç dock'tan çıktı: bastırma biter.
pub fn on_dock_leave() {
    with_hover(|h| h.suppress = -1);
}

/// Pencere listesi değişti: açık önizlemeyi aynı uygulama için yeniler.
fn refresh_preview() {
    if !crate::preview::is_open() {
        return;
    }
    let Some(Some(source)) = with_hover(|h| h.shown.clone()) else {
        return;
    };
    let Some(idx) = with(|s| s.list.iter().position(|a| a.icon_source == source)) else {
        return;
    };
    match idx {
        Some(i) => show_preview(i as i32),
        None => hide_preview(),
    }
}
