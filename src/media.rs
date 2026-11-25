//! Çalan medya (GSMTC) izleyici: Spotify, tarayıcılar vb. Olay tabanlı, polling yok.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
    ExifOrientationMode,
};
use windows::Media::Control::{
    CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus, MediaPropertiesChangedEventArgs,
    PlaybackInfoChangedEventArgs, TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::{IRandomAccessStream, IRandomAccessStreamReference};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaState {
    /// Başlığı olan bir güncel oturum var.
    pub active: bool,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// `updated_at` anındaki konum (ms); çalıyorsa geçen süre eklenmiş. UI araya enterpolasyon yapar.
    pub position_ms: u64,
    /// 0 = bilinmiyor (UI ilerleme çubuğunu gizler).
    pub duration_ms: u64,
    pub updated_at: Option<Instant>,
    pub can_prev: bool,
    pub can_next: bool,
}

/// Düz (premultiplied olmayan) RGBA8, yukarıdan aşağı; uzun kenar <= 128 px.
pub struct ArtPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

enum Msg {
    /// Güncel oturum değişti.
    Session,
    /// Medya özellikleri (başlık/kapak) değişti.
    Props,
    /// Oynatma bilgisi / zaman çizelgesi değişti.
    State,
}

type StateCb = Box<dyn Fn(MediaState) + Send + Sync>;
type ArtCb = Box<dyn Fn(Option<ArtPixels>) + Send + Sync>;

/// Oturum olay abonelikleri (token'lar kaldırmak için).
struct Subs {
    session: GlobalSystemMediaTransportControlsSession,
    media: i64,
    playback: i64,
    timeline: i64,
}

struct Worker {
    manager: GlobalSystemMediaTransportControlsSessionManager,
    tx: Sender<Msg>,
    on_state: StateCb,
    on_art: ArtCb,
    sub: Option<Subs>,
    title: String,
    artist: String,
    art_key: Option<String>,
    art_present: bool,
    last: Option<MediaState>,
}

/// Arka plan thread'inde izlemeyi başlatır; ilk durumu ve her değişikliği bildirir.
pub fn start(
    on_state: impl Fn(MediaState) + Send + Sync + 'static,
    on_art: impl Fn(Option<ArtPixels>) + Send + Sync + 'static,
) {
    let on_state: StateCb = Box::new(on_state);
    let on_art: ArtCb = Box::new(on_art);
    let _ = std::thread::Builder::new()
        .name("media".into())
        .spawn(move || {
            init_mta();
            let (tx, rx) = channel();
            let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                .and_then(|a| a.get())
            {
                Ok(m) => m,
                Err(_) => {
                    on_state(MediaState::default());
                    on_art(None);
                    return;
                }
            };
            let txc = tx.clone();
            let _ = manager.CurrentSessionChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSessionManager,
                CurrentSessionChangedEventArgs,
            >::new(move |_, _| {
                let _ = txc.send(Msg::Session);
                Ok(())
            }));
            let mut w = Worker {
                manager,
                tx,
                on_state,
                on_art,
                sub: None,
                title: String::new(),
                artist: String::new(),
                art_key: None,
                art_present: false,
                last: None,
            };
            w.on_session_changed();
            w.run(rx);
        });
}

fn init_mta() {
    // Zaten başlatılmışsa hata döner; yok sayılır.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
    }
}

impl Worker {
    fn run(&mut self, rx: Receiver<Msg>) {
        while let Ok(first) = rx.recv() {
            // Olay patlamalarını birleştir; en kapsamlı olay kazanır.
            let mut level = rank(&first);
            while let Ok(m) = rx.try_recv() {
                level = level.max(rank(&m));
            }
            match level {
                2 => self.on_session_changed(),
                1 => self.on_props_changed(),
                _ => self.publish(),
            }
        }
    }

    fn unsubscribe(&mut self) {
        if let Some(s) = self.sub.take() {
            let _ = s.session.RemoveMediaPropertiesChanged(s.media);
            let _ = s.session.RemovePlaybackInfoChanged(s.playback);
            let _ = s.session.RemoveTimelinePropertiesChanged(s.timeline);
        }
    }

    fn subscribe(&mut self, session: GlobalSystemMediaTransportControlsSession) {
        let t1 = self.tx.clone();
        let t2 = self.tx.clone();
        let t3 = self.tx.clone();
        let media = session.MediaPropertiesChanged(&TypedEventHandler::<
            GlobalSystemMediaTransportControlsSession,
            MediaPropertiesChangedEventArgs,
        >::new(move |_, _| {
            let _ = t1.send(Msg::Props);
            Ok(())
        }));
        let playback = session.PlaybackInfoChanged(&TypedEventHandler::<
            GlobalSystemMediaTransportControlsSession,
            PlaybackInfoChangedEventArgs,
        >::new(move |_, _| {
            let _ = t2.send(Msg::State);
            Ok(())
        }));
        let timeline = session.TimelinePropertiesChanged(&TypedEventHandler::<
            GlobalSystemMediaTransportControlsSession,
            TimelinePropertiesChangedEventArgs,
        >::new(move |_, _| {
            let _ = t3.send(Msg::State);
            Ok(())
        }));
        self.sub = Some(Subs {
            session,
            media: media.unwrap_or(0),
            playback: playback.unwrap_or(0),
            timeline: timeline.unwrap_or(0),
        });
    }

    fn on_session_changed(&mut self) {
        self.unsubscribe();
        if let Ok(s) = self.manager.GetCurrentSession() {
            self.subscribe(s);
        }
        self.on_props_changed();
    }

    /// Başlık/sanatçı yeniden okunur, durum yayınlanır, gerekirse kapak çözülür.
    fn on_props_changed(&mut self) {
        let mut thumb = None;
        self.title.clear();
        self.artist.clear();
        if let Some(sub) = &self.sub {
            if let Ok(p) = sub
                .session
                .TryGetMediaPropertiesAsync()
                .and_then(|a| a.get())
            {
                self.title = p.Title().map(|s| s.to_string()).unwrap_or_default();
                self.artist = p.Artist().map(|s| s.to_string()).unwrap_or_default();
                thumb = p.Thumbnail().ok();
            }
        }
        self.publish();

        // Kapak: parça değişmediyse (ve kapak zaten varsa) tekrar çözme.
        let key = if self.title.is_empty() {
            None
        } else {
            Some(format!("{}\u{0}{}", self.title, self.artist))
        };
        if key == self.art_key && (self.art_present || key.is_none()) {
            return;
        }
        let art = thumb.and_then(|t| decode_art(&t).ok());
        self.art_key = key;
        self.art_present = art.is_some();
        (self.on_art)(art);
    }

    fn build_state(&self) -> MediaState {
        let Some(sub) = &self.sub else {
            return MediaState::default();
        };
        if self.title.is_empty() {
            return MediaState::default();
        }
        let mut st = MediaState {
            active: true,
            title: self.title.clone(),
            artist: self.artist.clone(),
            ..Default::default()
        };
        if let Ok(info) = sub.session.GetPlaybackInfo() {
            st.playing = info.PlaybackStatus().ok()
                == Some(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
            if let Ok(c) = info.Controls() {
                st.can_prev = c.IsPreviousEnabled().unwrap_or(false);
                st.can_next = c.IsNextEnabled().unwrap_or(false);
            }
        }
        if let Ok(tl) = sub.session.GetTimelineProperties() {
            let start = tl.StartTime().map(|d| d.Duration).unwrap_or(0);
            let end = tl.EndTime().map(|d| d.Duration).unwrap_or(0);
            let pos = tl.Position().map(|d| d.Duration).unwrap_or(0);
            let last_upd = tl.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0);
            let dur_ms = ((end - start).max(0) / 10_000) as u64;
            let mut pos_ms = ((pos - start).max(0) / 10_000) as u64;
            if st.playing && last_upd > 0 {
                let elapsed = (now_filetime() - last_upd).max(0) / 10_000;
                pos_ms += elapsed as u64;
            }
            if dur_ms > 0 {
                pos_ms = pos_ms.min(dur_ms);
            }
            st.position_ms = pos_ms;
            st.duration_ms = dur_ms;
        }
        st.updated_at = Some(Instant::now());
        st
    }

    fn publish(&mut self) {
        let st = self.build_state();
        if let Some(prev) = &self.last {
            // Önceki konumun şimdiye ekstrapolasyonu.
            let expected = if prev.playing {
                let el = prev
                    .updated_at
                    .map_or(0, |t| t.elapsed().as_millis() as u64);
                prev.position_ms + el
            } else {
                prev.position_ms
            };
            let jump = st.position_ms.abs_diff(expected) > 1500;
            let same = prev.active == st.active
                && prev.title == st.title
                && prev.artist == st.artist
                && prev.playing == st.playing
                && prev.duration_ms == st.duration_ms
                && prev.can_prev == st.can_prev
                && prev.can_next == st.can_next;
            if same && !jump {
                return;
            }
        }
        self.last = Some(st.clone());
        (self.on_state)(st);
    }
}

fn rank(m: &Msg) -> u8 {
    match m {
        Msg::Session => 2,
        Msg::Props => 1,
        Msg::State => 0,
    }
}

/// Şimdiki zaman, 1601'den beri 100 ns (DateTime.UniversalTime ile aynı ölçek).
fn now_filetime() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    (d.as_nanos() / 100) as i64 + 116_444_736_000_000_000
}

/// Kapağı çözer; uzun kenar en fazla 128 px, straight RGBA8.
fn decode_art(thumb: &IRandomAccessStreamReference) -> windows::core::Result<ArtPixels> {
    let stream = thumb.OpenReadAsync()?.get()?;
    let stream: IRandomAccessStream = windows::core::Interface::cast(&stream)?;
    let dec = BitmapDecoder::CreateAsync(&stream)?.get()?;
    let (w, h) = (dec.PixelWidth()?, dec.PixelHeight()?);
    if w == 0 || h == 0 {
        return Err(windows::core::Error::empty());
    }
    let scale = (128.0 / w.max(h) as f64).min(1.0);
    let sw = ((w as f64 * scale).round() as u32).max(1);
    let sh = ((h as f64 * scale).round() as u32).max(1);
    let tr = BitmapTransform::new()?;
    tr.SetScaledWidth(sw)?;
    tr.SetScaledHeight(sh)?;
    let data = dec
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Straight,
            &tr,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .get()?;
    let rgba = data.DetachPixelData()?.to_vec();
    if rgba.len() != (sw * sh * 4) as usize {
        return Err(windows::core::Error::empty());
    }
    Ok(ArtPixels {
        width: sw,
        height: sh,
        rgba,
    })
}

/// Güncel oturuma bir komut gönderir; çağıranı bloklamaz.
fn control(f: impl FnOnce(&GlobalSystemMediaTransportControlsSession) + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("media-ctl".into())
        .spawn(move || {
            init_mta();
            let session = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                .and_then(|a| a.get())
                .and_then(|m| m.GetCurrentSession());
            if let Ok(s) = session {
                f(&s);
            }
        });
}

pub fn play_pause() {
    control(|s| {
        let _ = s.TryTogglePlayPauseAsync().and_then(|a| a.get());
    });
}

pub fn next() {
    control(|s| {
        let _ = s.TrySkipNextAsync().and_then(|a| a.get());
    });
}

pub fn previous() {
    control(|s| {
        let _ = s.TrySkipPreviousAsync().and_then(|a| a.get());
    });
}
