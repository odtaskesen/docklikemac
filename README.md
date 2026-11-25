# Dock — Windows 11 için macOS tarzı dock

Windows görev çubuğunun yerine geçen, **Rust + [Slint](https://slint.dev)** ile yazılmış, hafif bir macOS tarzı dock. Boştayken işlemci kullanımı neredeyse sıfır: sürekli çizim döngüsü yok, her şey olay tabanlı.

> *A lightweight macOS‑style dock for Windows 11, written in Rust + Slint. It replaces the taskbar, idles at ~0% CPU, and is fully event‑driven. UI language: Turkish.*

## Özellikler

**Uygulamalar**
- Sabitlenmiş ve açık uygulamalar tek sırada; ilk açılışta görev çubuğu sabitleri bir kez aktarılır.
- Sürükle‑bırak ile sıralama, dock dışına ya da çöp kutusuna sürükleyerek sabitlikten çıkarma.
- Açık uygulamanın altında gösterge çizgisi; penceresi olmayıp arka planda çalışan sabitlerde kırmızı çizgi.
- Chrome / Edge profilleri ayrı ikonlarla görünür.
- Kapalı bir uygulama başlatılınca ikon, penceresi gelene kadar zıplar.
- **Pencere önizlemesi:** ikonun üzerinde durunca açık pencerelerin canlı küçük görüntüleri (DWM thumbnail); tıklayınca o pencereye geçer, × ya da orta tık kapatır.
- Orta tık / Shift+tık yeni pencere açar; fare tekerleği o uygulamanın pencereleri arasında gezinir.
- İkonların üzerinde macOS tarzı büyüme (oranı ayarlanabilir).

**Widget'lar ve sistem alanı**
- Müzik (Windows medya oturumu: kapak, başlık, ilerleme, önceki/oynat/sonraki)
- Hava durumu ([Open‑Meteo](https://open-meteo.com), 30 dakikada bir)
- CPU / RAM halkaları
- Ağ, ses ve pil; ses simgesi üzerinde tekerlek ile yüzdeli ses kaydırıcısı
- Saat ve tarih, bildirim zili, tray taşma oku

**Yığınlar ve Geri Dönüşüm Kutusu**
- Dock'a klasör sabitlenir (varsayılan: İndirilenler). Tıklayınca içerik, en yeni önce, küçük resimli bir ızgarada açılır.
- Gezgin'den dock'a bırakılan klasör yığın olur.
- Geri Dönüşüm Kutusu boş/dolu ikonuyla en sağda; üstüne bırakılan dosyalar geri dönüşüm kutusuna gider, sağ tıkta "Boşalt".

**Pencere davranışı**
- Ana ekranda altta ortalı, ekranda yer ayırır (maximize pencereler dock'un altına girmez).
- Otomatik gizleme (`Ctrl+Alt+D` ya da menüden); tam ekran uygulamalarda kendiliğinden gizlenir.
- Görünüm: Şeffaf, Cam (Acrylic) ya da Koyu.
- Dock boyutu %75–%125 arasında canlı ayarlanır.
- Dock kapanırken ya da çökerken Windows görev çubuğu mutlaka geri gelir.
- "Windows ile Başlat" seçeneği.

**Ayarlar penceresi** — dock'un boş yerine sağ tık → *Ayarlar…*: boyut, büyütme ve büyüme oranı, görünüm, otomatik gizleme, Windows ile başlatma ve her widget'ı ayrı ayrı açıp kapatma.

## Kaynak kullanımı

Release derlemesi, boştayken (müzik çalarken) ölçüldü:

| | |
|---|---|
| CPU | ~%0.1 |
| Çalışma kümesi | ~117 MB |
| Özel bellek | ~83 MB |
| Exe | ~11.5 MB, ek çalışma zamanı gerekmez |

Çizim GPU ile yapılır (femtovg). Dock gizliyken zamanlayıcılar çalışmaz.

## Kurulum

Gereken: Windows 11, [Rust](https://rustup.rs) (stable, MSVC araç zinciri) ve Visual Studio Build Tools (C++).

```powershell
git clone https://github.com/odtaskesen/docklikemac.git
cd docklikemac
cargo build --release
.\target\release\dock.exe
```

İlk açılışta "Windows ile Başlat" açılır (Run anahtarına bu exe'nin yolu yazılır). Ayarlar menüsünden kapatılabilir.

Bir şey ters giderse ve görev çubuğu gizli kalırsa:

```powershell
.\target\release\dock.exe --restore
```

## Kullanım

| Ne | Nasıl |
|---|---|
| Uygulamayı aç / öne getir | Tık |
| Yeni pencere | Orta tık ya da Shift+tık |
| Pencereler arasında geç | İkon üzerinde fare tekerleği |
| Uygulama menüsü (sabitle, kapat…) | İkona sağ tık |
| Dock menüsü (görünüm, boyut, ayarlar, çıkış) | Boş yere sağ tık |
| Otomatik gizlemeyi aç/kapat | `Ctrl+Alt+D` |
| Ses | Ses simgesi üzerinde tekerlek |
| Klasörü yığın olarak ekle | Gezgin'den dock'a sürükle |

Ayarlar `%APPDATA%\Dock\config.json` dosyasında tutulur. Hava durumu şimdilik Ankara için (`src/weather.rs`).

## Proje yapısı

| Dosya | Görev |
|---|---|
| `src/main.rs` | Başlatma, tek kopya, çökme/çıkış temizliği |
| `src/dock.rs` | Konum, yer ayırma, otomatik gizleme, büyüme, dock menüsü |
| `src/apps.rs`, `src/tracker.rs` | Uygulama listesi, sabitler, pencere takibi (WinEvent) |
| `src/widgets.rs`, `media.rs`, `sysstatus.rs`, `weather.rs` | Widget verileri |
| `src/preview.rs`, `stack_grid.rs`, `size_panel.rs`, `popup.rs` | Dock üstü Win32 + DWM açılır pencereler |
| `src/stacks.rs`, `recycle.rs` | Yığınlar, Geri Dönüşüm Kutusu |
| `src/settings.rs`, `ui/settings.slint` | Ayarlar penceresi |
| `ui/dock.slint`, `ui/widgets.slint` | Dock arayüzü |
| `tools/` | Geliştirme sırasında kullanılan ölçüm betikleri (ekran koordinatları geliştiricinin bilgisayarına göre) |

Tasarım kararları ve geliştirme geçmişi [PLAN.md](PLAN.md)'de, teknik notlar [CLAUDE.md](CLAUDE.md)'de.
