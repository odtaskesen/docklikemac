//! Sistem durumu: CPU/RAM, ağ, ses seviyesi, pil. Olay tabanlı, yoklama thread'i yok.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::{implement, PCWSTR};
use windows::Networking::Connectivity::{
    NetworkConnectivityLevel, NetworkInformation, NetworkStatusChangedEventHandler,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::GetSystemTimes;

// ---------------------------------------------------------------- CPU / RAM

/// CPU ve RAM kullanımı (0.0..=1.0).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuRam {
    pub cpu: f32,
    pub ram: f32,
}

/// Ucuz örnekleyici; iki çağrı arasındaki GetSystemTimes farkını kullanır.
pub struct CpuSampler {
    prev: Option<(u64, u64, u64)>, // (idle, kernel, user)
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuSampler {
    pub fn new() -> Self {
        Self { prev: read_times() }
    }

    /// İlk çağrı cpu=0 döner (önceki ölçüm yoksa); sonrakiler delta üzerinden hesaplanır.
    pub fn sample(&mut self) -> CpuRam {
        let mut out = CpuRam { cpu: 0.0, ram: read_ram() };
        let now = read_times();
        if let (Some((pi, pk, pu)), Some((ni, nk, nu))) = (self.prev, now) {
            let idle = ni.wrapping_sub(pi);
            // kernel süresi idle'ı da içerir
            let total = nk.wrapping_sub(pk).wrapping_add(nu.wrapping_sub(pu));
            if total > 0 {
                out.cpu = (1.0 - idle as f64 / total as f64).clamp(0.0, 1.0) as f32;
            }
        }
        if now.is_some() {
            self.prev = now;
        }
        out
    }
}

fn read_times() -> Option<(u64, u64, u64)> {
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut i, mut k, mut u) = (z, z, z);
    // SAFETY: geçerli çıktı işaretçileri.
    let ok = unsafe { GetSystemTimes(&mut i, &mut k, &mut u) };
    (ok != 0).then(|| (ft(i), ft(k), ft(u)))
}

fn read_ram() -> f32 {
    // SAFETY: yapı sıfırlanıp dwLength dolduruluyor.
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m) == 0 {
            return 0.0;
        }
        (m.dwMemoryLoad as f32 / 100.0).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------- Pil

/// Pil durumu.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BatteryState {
    pub present: bool,
    pub percent: u8,
    pub charging: bool,
}

/// GetSystemPowerStatus; pil yoksa/bilinmiyorsa present=false.
pub fn battery() -> BatteryState {
    // SAFETY: geçerli çıktı yapısı.
    unsafe {
        let mut s: SYSTEM_POWER_STATUS = std::mem::zeroed();
        if GetSystemPowerStatus(&mut s) == 0 {
            return BatteryState::default();
        }
        let present = s.BatteryFlag & 128 == 0 && s.BatteryFlag != 255;
        BatteryState {
            present,
            percent: if present && s.BatteryLifePercent <= 100 { s.BatteryLifePercent } else { 0 },
            charging: s.ACLineStatus == 1,
        }
    }
}

// ---------------------------------------------------------------- Ağ

/// Ağ durumu.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NetState {
    pub connected: bool,
    pub wifi: bool,
    /// Wi-Fi çubukları 0..=4.
    pub signal: u8,
}

fn read_net() -> NetState {
    let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else {
        return NetState::default();
    };
    let connected = profile
        .GetNetworkConnectivityLevel()
        .map(|l| l != NetworkConnectivityLevel::None)
        .unwrap_or(false);
    let wifi = profile.IsWlanConnectionProfile().unwrap_or(false);
    let signal = if wifi {
        profile
            .GetSignalBars()
            .ok()
            .and_then(|r| r.Value().ok())
            .map(|b| b.min(4))
            .unwrap_or(0)
    } else {
        0
    };
    NetState { connected, wifi, signal }
}

/// NetworkStatusChanged olayına abone olur; hemen ve her değişimde `on_change` çağrılır.
pub fn start_network(on_change: impl Fn(NetState) + Send + Sync + 'static) {
    let last: Mutex<Option<NetState>> = Mutex::new(None);
    let emit = Arc::new(move || {
        let now = read_net();
        let mut g = last.lock().unwrap_or_else(|e| e.into_inner());
        if *g != Some(now) {
            *g = Some(now);
            drop(g);
            on_change(now);
        }
    });
    emit();
    let e2 = emit.clone();
    let handler = NetworkStatusChangedEventHandler::new(move |_| {
        e2();
        Ok(())
    });
    // Abonelik süreç boyunca açık kalır (token bilerek tutulmuyor).
    let _ = NetworkInformation::NetworkStatusChanged(&handler);
}

// ---------------------------------------------------------------- Ses

/// Varsayılan çıkış cihazının ana ses seviyesi.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VolumeState {
    pub level: f32,
    pub muted: bool,
}

struct Emitter {
    cb: Box<dyn Fn(VolumeState) + Send + Sync>,
    last: Mutex<Option<VolumeState>>,
}

impl Emitter {
    fn emit(&self, s: VolumeState) {
        let mut g = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = *g {
            if p.muted == s.muted && (p.level - s.level).abs() < 0.005 {
                return;
            }
        }
        *g = Some(s);
        drop(g);
        (self.cb)(s);
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolCb {
    em: Arc<Emitter>,
}

impl IAudioEndpointVolumeCallback_Impl for VolCb_Impl {
    fn OnNotify(&self, pnotify: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        // SAFETY: Windows geçerli bir veri işaretçisi verir.
        if let Some(d) = unsafe { pnotify.as_ref() } {
            self.em.emit(VolumeState {
                level: d.fMasterVolume.clamp(0.0, 1.0),
                muted: d.bMuted.as_bool(),
            });
        }
        Ok(())
    }
}

/// Ses iş parçacığına giden istekler.
enum VolMsg {
    /// Varsayılan cihaz değişti: yeniden bağlan.
    Reconnect,
    /// Sesi bu seviyeye ayarla (0.0..=1.0).
    Set(f32),
}

fn send_vol(msg: VolMsg) {
    if let Some(tx) = VOL_TX.get() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(msg);
        }
    }
}

/// Ses seviyesini ayarlar (0.0..=1.0). Sıfırdan büyükse sessizi kaldırır.
pub fn set_volume(level: f32) {
    send_vol(VolMsg::Set(level));
}

static VOL_TX: OnceLock<Mutex<Sender<VolMsg>>> = OnceLock::new();

#[implement(IMMNotificationClient)]
struct DevNotif {
    tx: Mutex<Sender<VolMsg>>,
}

impl IMMNotificationClient_Impl for DevNotif_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _id: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            // Yeniden bağlanma işi ayrı thread'de yapılır.
            if let Ok(tx) = self.tx.lock() {
                let _ = tx.send(VolMsg::Reconnect);
            }
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Bağlı uç nokta + callback (ayrılırken kaydı silmek için).
struct Attached {
    ep: IAudioEndpointVolume,
    cb: IAudioEndpointVolumeCallback,
}

impl Drop for Attached {
    fn drop(&mut self) {
        // SAFETY: aynı callback ile kayıt silinir.
        unsafe {
            let _ = self.ep.UnregisterControlChangeNotify(&self.cb);
        }
    }
}

fn attach(en: &IMMDeviceEnumerator, em: &Arc<Emitter>) -> Option<Attached> {
    // SAFETY: COM çağrıları, hatalar None'a çevrilir.
    unsafe {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let ep: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).ok()?;
        let cb: IAudioEndpointVolumeCallback = VolCb { em: em.clone() }.into();
        ep.RegisterControlChangeNotify(&cb).ok()?;
        if let (Ok(level), Ok(muted)) = (ep.GetMasterVolumeLevelScalar(), ep.GetMute()) {
            em.emit(VolumeState { level: level.clamp(0.0, 1.0), muted: muted.as_bool() });
        }
        Some(Attached { ep, cb })
    }
}

/// Ses olaylarına abone olur (özel MTA thread'i; olay gelene kadar bloklu uyur).
pub fn start_volume(on_change: impl Fn(VolumeState) + Send + Sync + 'static) {
    let em = Arc::new(Emitter { cb: Box::new(on_change), last: Mutex::new(None) });
    let _ = std::thread::Builder::new().name("volume".into()).spawn(move || {
        // SAFETY: bu thread'e özel COM başlatma; thread süreç boyunca yaşar.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
            else {
                return;
            };
            let (tx, rx) = channel::<VolMsg>();
            let _ = VOL_TX.set(Mutex::new(tx.clone()));
            let client: IMMNotificationClient = DevNotif { tx: Mutex::new(tx) }.into();
            let _ = en.RegisterEndpointNotificationCallback(&client);
            let mut cur = attach(&en, &em);
            // Varsayılan cihaz değişince yeniden bağlan; olay yoksa bloklu bekler.
            while let Ok(msg) = rx.recv() {
                match msg {
                    VolMsg::Reconnect => {
                        drop(cur.take()); // eski kaydı sil
                        cur = attach(&en, &em);
                    }
                    VolMsg::Set(v) => {
                        if let Some(a) = &cur {
                            let _ = a.ep.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null());
                            if v > 0.0 {
                                let _ = a.ep.SetMute(false, std::ptr::null());
                            }
                        }
                    }
                }
            }
            drop(cur);
        }
    });
}
