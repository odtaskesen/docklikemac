//! Görev çubuğunda görünecek üst düzey uygulama pencerelerini izler.
//! Olay tabanlı (SetWinEventHook) çalışır; boşta CPU kullanmaz.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND as ComHwnd;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED,
};
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
    PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, SHGetPropertyStoreForWindow,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::UI::Shell::SHLoadIndirectString;

use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, TRUE};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows_sys::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_MENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, DispatchMessageW,
    EnumWindows, FindWindowExW, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetMessageW, GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, KillTimer, PostMessageW,
    SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage, CHILDID_SELF,
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, GA_ROOTOWNER, GWL_EXSTYLE, GWL_STYLE, GW_OWNER, MSG, OBJID_WINDOW,
    SW_MINIMIZE, SW_RESTORE, WINEVENT_OUTOFCONTEXT, WM_CLOSE, WM_TIMER, WS_CHILD,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// Debounce süresi (ms).
const DEBOUNCE_MS: u32 = 50;

/// AUMID'li ama yeniden başlatma özelliği olmayan pencerelerde, özellikler geç
/// yazılırsa yakalamak için yeniden okuma süresi.
const PROPS_RETRY_WINDOW: Duration = Duration::from_secs(5);
/// Aynı pencere için en fazla yeniden okuma sayısı.
const PROPS_RETRY_MAX: u32 = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct AppWindow {
    pub hwnd: isize,
    pub title: String,
    /// Tam exe yolu, küçük harfli. UWP için gerçek uygulama exe'si (CoreWindow sürecinden).
    pub exe: String,
    /// AppUserModelID (varsa).
    pub aumid: Option<String>,
    pub pid: u32,
    pub minimized: bool,
    /// Pencerenin yeniden başlatma ikonu (ör. Chrome profil rozetli .ico), ham değer (indeks dahil).
    pub relaunch_icon: Option<String>,
    /// Yeniden başlatma görünen adı (ör. "Ad - Chrome"), `@` kaynakları çözülmüş.
    pub relaunch_name: Option<String>,
    /// O profili açan komut satırı.
    pub relaunch_command: Option<String>,
    /// Uygulamayı başlatmak için doğru hedef, `exe`den farklıysa: paketli (Store)
    /// uygulamada `shell:AppsFolder\<AUMID>`, yardımcı süreçte (ör. steamwebhelper)
    /// aynı klasör ağacındaki üst sürecin exe'si (steam.exe).
    pub launch: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Z sırasına göre, en üstteki ilk.
    pub windows: Vec<AppWindow>,
    /// Ön plandaki pencere `windows` içindeyse hwnd'si, değilse 0.
    pub foreground: isize,
}

/// Pencere başına önbelleğe alınan, değişmeyen bilgiler.
#[derive(Clone)]
struct CacheEntry {
    pid: u32,
    exe: String,
    aumid: Option<String>,
    relaunch_icon: Option<String>,
    relaunch_name: Option<String>,
    relaunch_command: Option<String>,
    launch: Option<String>,
    /// Girişin ilk oluşturulduğu an (özellik yeniden okuma penceresi için).
    first_seen: Instant,
    /// Yapılan yeniden okuma sayısı.
    retries: u32,
}

impl CacheEntry {
    fn has_relaunch(&self) -> bool {
        self.relaunch_icon.is_some() || self.relaunch_name.is_some() || self.relaunch_command.is_some()
    }

    /// AUMID var, yeniden başlatma özellikleri henüz yok ve süre dolmadı.
    fn wants_props_retry(&self) -> bool {
        self.aumid.is_some()
            && !self.has_relaunch()
            && self.retries < PROPS_RETRY_MAX
            && self.first_seen.elapsed() < PROPS_RETRY_WINDOW
    }

    fn apply(&mut self, p: WindowProps) {
        self.aumid = p.aumid;
        self.relaunch_icon = p.relaunch_icon;
        self.relaunch_name = p.relaunch_name;
        self.relaunch_command = p.relaunch_command;
    }
}

/// Pencere özellik deposundan okunan değerler.
#[derive(Default)]
struct WindowProps {
    aumid: Option<String>,
    relaunch_icon: Option<String>,
    relaunch_name: Option<String>,
    relaunch_command: Option<String>,
}

thread_local! {
    static CACHE: RefCell<HashMap<isize, CacheEntry>> = RefCell::new(HashMap::new());
    static COM_READY: Cell<bool> = const { Cell::new(false) };
    /// exe → başlatma hedefi (bir kez hesaplanır).
    static LAUNCHERS: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
    static TRACKER: RefCell<Option<TrackerState>> = const { RefCell::new(None) };
    static DIRTY: Cell<bool> = const { Cell::new(false) };
}

struct TrackerState {
    on_change: Box<dyn Fn(Snapshot)>,
    last: Option<Snapshot>,
    /// Bekleyen debounce zamanlayıcısı (0 = yok).
    timer: usize,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Bu iş parçacığında COM'u bir kez başlatır (RPC_E_CHANGED_MODE yok sayılır).
fn ensure_com() {
    COM_READY.with(|c| {
        if !c.get() {
            // SAFETY: tek seferlik COM başlatma; hata yok sayılır.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            c.set(true);
        }
    });
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: buf geçerli ve 256 eleman.
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: hwnd geçersizse çağrılar 0 döner.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: cloaked u32 boyutunda geçerli bir çıktı alanı.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    hr >= 0 && cloaked != 0
}

/// Sürecin tam exe yolu (küçük harfli).
fn exe_of_pid(pid: u32) -> Option<String> {
    // SAFETY: handle kullanım sonrası kapatılır.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
    }
}

/// Özellik deposundan bir dize değeri okur; boş dize `None` sayılır.
fn prop_string(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: store geçerli bir COM nesnesi; dönen string CoTaskMemFree ile serbest bırakılır.
    unsafe {
        let pv = store.GetValue(key).ok()?;
        let p = PropVariantToStringAlloc(&pv).ok()?;
        if p.is_null() {
            return None;
        }
        let s = PCWSTR(p.0).to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

/// `@dll,-123` gibi dolaylı dize kaynaklarını çözer; diğerlerini olduğu gibi döndürür.
fn resolve_indirect(s: String) -> String {
    if !s.starts_with('@') {
        return s;
    }
    let src = wide(&s);
    let mut buf = [0u16; 512];
    // SAFETY: src NUL sonlu, buf geçerli ve yazılabilir.
    let ok = unsafe { SHLoadIndirectString(PCWSTR(src.as_ptr()), &mut buf, None) }.is_ok();
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if !ok || n == 0 {
        return s;
    }
    String::from_utf16_lossy(&buf[..n])
}

/// Pencerenin AUMID'ini ve yeniden başlatma özelliklerini tek depo erişimiyle okur.
fn window_props(hwnd: HWND) -> WindowProps {
    ensure_com();
    // SAFETY: COM başlatıldı; hwnd geçersizse çağrı hata döner.
    let store: Option<IPropertyStore> =
        unsafe { SHGetPropertyStoreForWindow(ComHwnd(hwnd as *mut _)) }.ok();
    let Some(store) = store else {
        return WindowProps::default();
    };
    WindowProps {
        aumid: prop_string(&store, &PKEY_AppUserModel_ID),
        relaunch_icon: prop_string(&store, &PKEY_AppUserModel_RelaunchIconResource),
        relaunch_name: prop_string(&store, &PKEY_AppUserModel_RelaunchDisplayNameResource)
            .map(resolve_indirect),
        relaunch_command: prop_string(&store, &PKEY_AppUserModel_RelaunchCommand),
    }
}

/// ApplicationFrameWindow içindeki CoreWindow'un süreç kimliği.
fn core_window_pid(frame: HWND) -> Option<u32> {
    let cls = wide("Windows.UI.Core.CoreWindow");
    // SAFETY: cls NUL sonlu.
    unsafe {
        let child = FindWindowExW(frame, null_mut(), cls.as_ptr(), null());
        if child.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(child, &mut pid);
        (pid != 0).then_some(pid)
    }
}

/// Paketli sürecin AppUserModelID'si (paketli değilse None).
fn package_aumid(pid: u32) -> Option<String> {
    // SAFETY: tutamaç kullanım sonrası kapatılır; buf `len` kadar u16 yazılabilir.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let rc = GetApplicationUserModelId(h, &mut len, buf.as_mut_ptr());
        CloseHandle(h);
        if rc != 0 || len <= 1 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize - 1]))
    }
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('\\').map(|(d, _)| d).unwrap_or("")
}

/// Pencerenin sürecinden uygulamanın doğru başlatma hedefini bulur (bkz. `AppWindow::launch`).
pub fn find_launch(pid: u32, exe: &str) -> Option<String> {
    if exe.contains("\\windowsapps\\") {
        return package_aumid(pid)
            .or_else(|| crate::launch::packaged_aumid(exe))
            .map(|a| format!("shell:AppsFolder\\{a}"));
    }
    // Üst süreçlere çık: üst sürecin klasörü bu exe'nin klasörünü kapsıyorsa
    // (steam ⊃ steam\bin\cef\…) asıl uygulama odur.
    let parents = crate::procs::parent_pids();
    let mut buf = [0u16; 1024];
    let (mut cur_pid, mut cur_exe) = (pid, exe.to_string());
    let mut found = None;
    for _ in 0..4 {
        let Some(&ppid) = parents.get(&cur_pid) else { break };
        if ppid == 0 || ppid == cur_pid {
            break;
        }
        let Some(pexe) = crate::procs::image_path(ppid, &mut buf) else { break };
        let pdir = parent_dir(&pexe);
        if pdir.is_empty() || !parent_dir(&cur_exe).starts_with(pdir) {
            break;
        }
        if pexe != exe {
            found = Some(pexe.clone());
        }
        cur_pid = ppid;
        cur_exe = pexe;
    }
    found
}

/// `find_launch`'ın exe başına önbellekli hali.
fn launch_of(pid: u32, exe: &str) -> Option<String> {
    if let Some(v) = LAUNCHERS.with(|l| l.borrow().get(exe).cloned()) {
        return v;
    }
    let v = find_launch(pid, exe);
    LAUNCHERS.with(|l| l.borrow_mut().insert(exe.to_string(), v.clone()));
    v
}

/// Önbelleğe alınabilir bilgileri hesaplar; ikinci değer "kalıcı mı" demektir.
fn resolve_entry(hwnd: HWND, class: &str) -> Option<(CacheEntry, bool)> {
    let mut pid = 0u32;
    // SAFETY: pid geçerli çıktı alanı.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    let mut real_pid = pid;
    let mut stable = true;
    if class == "ApplicationFrameWindow" {
        match core_window_pid(hwnd) {
            Some(p) => real_pid = p,
            // CoreWindow henüz yok; sonra tekrar denenecek.
            None => stable = false,
        }
    }
    let exe = exe_of_pid(real_pid)?;
    let props = window_props(hwnd);
    let launch = launch_of(real_pid, &exe);
    if class == "ApplicationFrameWindow" && props.aumid.is_none() {
        stable = false;
    }
    let entry = CacheEntry {
        pid: real_pid,
        exe,
        aumid: props.aumid,
        relaunch_icon: props.relaunch_icon,
        relaunch_name: props.relaunch_name,
        relaunch_command: props.relaunch_command,
        launch,
        first_seen: Instant::now(),
        retries: 0,
    };
    Some((entry, stable))
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let v = &mut *(lparam as *mut Vec<isize>);
    v.push(hwnd as isize);
    TRUE
}

/// Pencere görev çubuğunda görünmeli mi (sınıf adını da döndürür).
fn is_taskbar_window(hwnd: HWND, own_pid: u32) -> Option<String> {
    // SAFETY: geçersiz hwnd'de çağrılar hata değeri döner.
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let app = ex & WS_EX_APPWINDOW != 0;
        if !GetWindow(hwnd, GW_OWNER).is_null() && !app {
            return None;
        }
        if ex & WS_EX_TOOLWINDOW != 0 && !app {
            return None;
        }
        if ex & WS_EX_NOACTIVATE != 0 && !app {
            return None;
        }
        if is_cloaked(hwnd) {
            return None;
        }
        let class = class_name(hwnd);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == own_pid {
            return None;
        }
        Some(class)
    }
}

/// Şu anki pencere listesini eşzamanlı olarak üretir.
pub fn snapshot() -> Snapshot {
    ensure_com();
    let mut handles: Vec<isize> = Vec::with_capacity(128);
    // SAFETY: enum_proc senkron çalışır, handles bu süre boyunca yaşar.
    unsafe {
        EnumWindows(Some(enum_proc), &mut handles as *mut Vec<isize> as LPARAM);
    }
    // SAFETY: basit sorgu.
    let own_pid = unsafe { GetCurrentProcessId() };

    let mut windows = Vec::new();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let mut seen = HashSet::new();
        for h in handles {
            let hwnd = h as HWND;
            let Some(class) = is_taskbar_window(hwnd, own_pid) else {
                continue;
            };
            seen.insert(h);
            let entry = match cache.get_mut(&h) {
                Some(e) => {
                    // Chrome/Edge özellikleri pencereden biraz sonra yazabilir; kısa süre yeniden oku.
                    if e.wants_props_retry() {
                        e.retries += 1;
                        e.apply(window_props(hwnd));
                    }
                    e.clone()
                }
                None => {
                    let Some((e, stable)) = resolve_entry(hwnd, &class) else {
                        continue;
                    };
                    if stable {
                        cache.insert(h, e.clone());
                    }
                    e
                }
            };
            windows.push(AppWindow {
                hwnd: h,
                title: window_title(hwnd),
                exe: entry.exe,
                aumid: entry.aumid,
                relaunch_icon: entry.relaunch_icon,
                relaunch_name: entry.relaunch_name,
                relaunch_command: entry.relaunch_command,
                launch: entry.launch,
                pid: entry.pid,
                // SAFETY: basit sorgu.
                minimized: unsafe { IsIconic(hwnd) } != 0,
            });
        }
        // Artık olmayan pencerelerin önbelleğini at.
        cache.retain(|k, _| seen.contains(k));
    });

    // SAFETY: basit sorgular.
    let foreground = unsafe {
        let fg = GetForegroundWindow();
        let mut found = fg as isize;
        if !windows.iter().any(|w| w.hwnd == found) {
            let root = if fg.is_null() { fg } else { GetAncestor(fg, GA_ROOTOWNER) };
            found = root as isize;
            if !windows.iter().any(|w| w.hwnd == found) {
                found = 0;
            }
        }
        found
    };
    Snapshot { windows, foreground }
}

/// Olay kancası: yalnızca pencere nesnesi olaylarına tepki verir, debounce başlatır.
unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_null() || id_object != OBJID_WINDOW || id_child != CHILDID_SELF as i32 {
        return;
    }
    // Alt pencere (WS_CHILD) olaylarını ucuzca ele.
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CHILD != 0 {
        return;
    }
    let _ = event;
    schedule();
}

/// Debounce zamanlayıcısını kurar. `flush` sırasında (COM çağrısı mesaj
/// pompalarken) yeniden girilirse durumu ödünç almaz, sadece işaretler.
fn schedule() {
    TRACKER.with(|t| match t.try_borrow_mut() {
        Ok(mut guard) => {
            if let Some(st) = guard.as_mut() {
                if st.timer == 0 {
                    // SAFETY: bu iş parçacığının zamanlayıcısı.
                    st.timer = unsafe { SetTimer(null_mut(), 0, DEBOUNCE_MS, None) };
                }
            }
        }
        Err(_) => DIRTY.with(|d| d.set(true)),
    });
}

/// Debounce süresi dolunca anlık görüntü alır, değiştiyse bildirir.
fn flush() {
    TRACKER.with(|t| {
        let mut guard = t.borrow_mut();
        let Some(st) = guard.as_mut() else { return };
        if st.timer != 0 {
            // SAFETY: bu iş parçacığının zamanlayıcısı.
            unsafe { KillTimer(null_mut(), st.timer) };
            st.timer = 0;
        }
        let snap = snapshot();
        if st.last.as_ref() != Some(&snap) {
            st.last = Some(snap.clone());
            (st.on_change)(snap);
        }
    });
    // Anlık görüntü alınırken gelen olaylar kaybolmasın.
    if DIRTY.with(|d| d.replace(false)) {
        schedule();
    }
}

/// Arka plan iş parçacığında olay izlemeyi başlatır; ilk anlık görüntüyle hemen bildirir.
pub fn start(on_change: impl Fn(Snapshot) + Send + 'static) {
    std::thread::Builder::new()
        .name("window-tracker".into())
        .spawn(move || {
            ensure_com();
            TRACKER.with(|t| {
                *t.borrow_mut() = Some(TrackerState {
                    on_change: Box::new(on_change),
                    last: None,
                    timer: 0,
                });
            });

            let events = [
                EVENT_OBJECT_SHOW,
                EVENT_OBJECT_HIDE,
                EVENT_OBJECT_DESTROY,
                EVENT_OBJECT_CLOAKED,
                EVENT_OBJECT_UNCLOAKED,
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_MINIMIZESTART,
                EVENT_SYSTEM_MINIMIZEEND,
            ];
            for ev in events {
                // SAFETY: win_event_proc geçerli bir WINEVENTPROC; kancalar iş parçacığı bitene dek yaşar.
                unsafe {
                    SetWinEventHook(ev, ev, null_mut(), Some(win_event_proc), 0, 0, WINEVENT_OUTOFCONTEXT);
                }
            }

            // İlk bildirim.
            flush();

            // SAFETY: standart mesaj döngüsü.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                    let ours = TRACKER.with(|t| {
                        t.borrow().as_ref().map(|s| s.timer).unwrap_or(0)
                    });
                    if msg.message == WM_TIMER && msg.hwnd.is_null() && ours != 0 && msg.wParam == ours {
                        flush();
                        continue;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        })
        .ok();
}

/// Pencereyi güvenilir biçimde öne getirir (küçültülmüşse önce geri yükler).
pub fn activate(hwnd: isize) {
    let h = hwnd as HWND;
    // SAFETY: Win32 çağrıları geçersiz hwnd'de hata döner, panik etmez.
    unsafe {
        if IsWindow(h) == 0 {
            return;
        }
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        AllowSetForegroundWindow(u32::MAX);
        if SetForegroundWindow(h) != 0 && GetForegroundWindow() == h {
            return;
        }
        // ALT tuşu hilesi: ön plan kilidini gevşetir.
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VK_MENU, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        };
        let inputs = [key(0), key(KEYEVENTF_KEYUP)];
        SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
        if SetForegroundWindow(h) != 0 && GetForegroundWindow() == h {
            return;
        }
        // Son çare: ön plan iş parçacığına bağlan.
        let fg = GetForegroundWindow();
        let fg_thread = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, null_mut()) };
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, 1) != 0;
        BringWindowToTop(h);
        SetForegroundWindow(h);
        if attached {
            AttachThreadInput(me, fg_thread, 0);
        }
    }
}

/// Pencereyi küçültür.
pub fn minimize(hwnd: isize) {
    // SAFETY: geçersiz hwnd zararsız.
    unsafe {
        ShowWindow(hwnd as HWND, SW_MINIMIZE);
    }
}

/// Pencereye kapatma isteği (WM_CLOSE) gönderir.
pub fn close(hwnd: isize) {
    // SAFETY: geçersiz hwnd zararsız.
    unsafe {
        PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
    }
}

/// Süreci zorla sonlandırır.
pub fn force_kill(pid: u32) -> bool {
    // SAFETY: handle kapatılır.
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}
