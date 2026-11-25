# Dock (docklikemac)

Windows 11 için macOS tarzı, düşük kaynak tüketen dock. Rust + Slint.

**Okuma sırası:** bu dosya, sonra [PLAN.md](PLAN.md)'nin "Durum" ve sıradaki adım bölümü. Bitmiş aşamaların ayrıntısını araştırma, kodu baştan okuma. Sadece işin dokunduğu dosyaları, gerekirse ilgili kısmıyla oku (Grep + satır aralığı).

## Kullanıcıyla çalışma

- Kullanıcıyla **Türkçe** konuş, arayüz metinleri de Türkçe.
- **Kurulumdan önce onay al** (rustup, Build Tools, crate dışı araçlar).
- Plan dışı bir karar gerekirse kullanıcıya sor, sonra kararı PLAN.md'ye yaz.
- Her alt adımın sonunda PLAN.md'deki tabloya bir satırlık sonuç yaz. Gruplar bitince dock'u çalıştırıp kullanıcıya göster; onay gelmeden sıradaki gruba geçme.
- Görsel bir sorun bildirilirse ("flaş", "takılma") tahminle düzeltme: ekranı kare kare kaydedip ölç (`tools\`), düzeltmeyi aynı ölçümle doğrula.

## Çalıştırma

- Yeni kabukta `cargo` yoksa PATH'e `$env:USERPROFILE\.cargo\bin` ekle.
- Ara kontrol: `cargo check` / `cargo clippy` (uyarısız olmalı). Denemeden önce `cargo build`. Release derlemesi sadece aşama sonunda.
- Çalışan dock exe'yi kilitler: derlemeden önce `tools\close_dock.ps1` ile kapat (WM_CLOSE, görev çubuğu geri gelir). `Stop-Process` kullanma; kullandıysan hemen `cargo run -- --restore`.
- Kullanıcının dock'u: `target\release\dock.exe`; Windows açılışında başlıyor (Run anahtarı). Release'i yeniden derlemek için de önce kapat.
- **Claude masaüstü uygulaması bir Store (MSIX) uygulaması:** buradaki kabuktan başlatılan her süreç sanal bir AppData ve HKCU görür (`%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Dock\config.json`). Ayrıca Claude kapanınca o süreçler de kapanır. Bu yüzden:
  - Dock'u her zaman `explorer.exe "C:\Users\PC\Desktop\docklikemac\target\release\dock.exe"` ile başlat. Gezgin üzerinden açılması ~10 sn sürebilir.
  - Gerçek ayarları ve Run anahtarını `explorer.exe "...\tools\real_config.bat"` ile oku: `tools\_real_config.json` ve `tools\_real_run.txt` dosyalarını yazar, okuduktan sonra sil. Kabuktan okunan `%APPDATA%\Dock\config.json` sanal kopyadır.
- `SLINT_BACKEND=winit-software` ile yazılım çizicisi denenebilir (sadece ölçüm için; karar GPU).
- Ayarlar: `%APPDATA%\Dock\config.json`. Hava önbelleği: `weather.json`.
- Ekran koordinatlı test betikleri bu bilgisayara göre (3440×1440, dock altta ortada): `close_dock.ps1`, `frames.ps1` (dock bölgesini kare kare kaydet), `fast_probe.ps1` (flaş sayar), `record_user.ps1` + `analyze_rec.ps1` (kullanıcının gerçek fare hareketini kaydet/analiz et).

## Kod haritası

| Dosya | Görev |
|---|---|
| `main.rs` | Başlatma, tek kopya muteksi, panic/Ctrl+C temizliği (`cleanup`), Slint geri çağrıları, `--restore` |
| `dock.rs` | Dock durumu: konum, yer ayırma, otomatik gizleme/kayma, tam ekran, dock menüsü (Windows ile Başlat dahil), büyüme (Rust zamanlayıcısı), cam arka plan penceresi, `shown()` |
| `apps.rs` | Uygulama listesi (sabitler + açık), tıklama, uygulama menüsü, sabitleme/sıralama/sürükle-bırak, ikon iş parçacığı, tray tespiti (`procs` iş parçacığı + 4 sn yoklama), Chrome profilleri |
| `tracker.rs` | Pencere takibi (SetWinEventHook, 50 ms birleştirme). `AppWindow`: hwnd, title, exe, aumid, pid, minimized, relaunch_icon/name/command |
| `procs.rs` | `running_exes_matching(names)`: Toolhelp ile sadece verilen exe adları |
| `icons.rs`, `launch.rs` | Kabuk ikonları, kısayol çözme, görünen ad, başlatma, Run anahtarı, vurgu rengi, görev çubuğu sabitleri |
| `widgets.rs` | Widget verilerini arayüze aktarır, widget tıklamaları |
| `media.rs`, `sysstatus.rs`, `weather.rs` | GSMTC müzik; CPU/RAM, ağ, ses, pil; Open-Meteo |
| `shell_thread.rs` | Ayrı mesaj döngüsü: Ctrl+Alt+D, görev çubuğu kancası, Win32 menüleri (`show_menu`), alt kenar şeridi |
| `appbar.rs`, `taskbar.rs`, `win.rs`, `config.rs` | AppBar, görev çubuğu gizle/geri getir, Win32 yardımcıları + alt sınıf, ayarlar (`PinnedApp`: icon, seen_exes) |
| `settings.rs`, `ui/settings.slint` | Ayarlar penceresi (normal Slint penceresi, sadece açıkken var); değişiklikleri `dock::update_config` ile uygular |
| `popup.rs`, `size_panel.rs` | Dock üstü Win32+DWM açılır pencere temeli; kaydırıcı paneli ("Boyut…" ve tekerlekle açılan ses, `Kind`) |
| `preview.rs`, `stack_grid.rs`, `stacks.rs` | Pencere önizlemesi (DWM thumbnail); yığın ızgarası (GDI); yığın klasör listesi, ikon iş parçacığı, `downloads_dir`. Yığınların dock tarafı `widgets.rs` sonunda |
| `ui/dock.slint`, `ui/widgets.slint`, `ui/icons/*.svg` | Arayüz. Yeni ölçüler `Npx * Z.k` (dock) ya da `Npx * Z.w` (widget) yazılır (dock boyutu ayarı) |

## Teknik dersler

- Slint `animate` dock uzun süre boşta kalınca anında bitiyor. Hissedilen geçişleri Rust'ta `slint::Timer` ile sür (`dock.rs` büyüme/kayma örnek).
- Dock penceresinin bölgesini ya da boyutunu çalışırken değiştirme: GPU penceresinde tek karelik flaş yapıyor. Dock'un üstünde açılacak her şey **ayrı pencere** olmalı.
- Dock üstü açılır pencereler Win32 + DWM (5a kararı, PLAN.md). Slint'te ikinci pencerenin `show()`'u her seferinde odak çalar; Win32 ile gizlenirse Slint onu görünür sayar ve olay döngüsü kapanmaz.
- Tüm fare olaylarını `dock.slint`'teki tek `TouchArea` alır; tıklanabilir yeni öğeler `widgets.slint`'teki `HitBox` / `Hit` düzenini kullanır.
- Alt sınıf (`win.rs`) dock durumuna doğrudan dokunmaz, `invoke_from_event_loop` ile erteler (RefCell yeniden girişi). Durum `with(|s| ...)` ile ödünç alınır.
- Slint'te `Rectangle`'dan türeyen bileşende `background` adlı özellik tanımlanamaz (`Slot`'taki tray özelliği bu yüzden `in-tray`).
- windows-sys: `CreateMutexW` için `Win32_Security`, Toolhelp için `Win32_System_Diagnostics_ToolHelp` özelliği gerekiyor. windows 0.61'de `PROPERTYKEY`, `Win32::Foundation` altında.
- Toolhelp süreç anlık görüntüsü ~3 ms sürüyor; UI iş parçacığında çağırma.
- Chrome'un Default profilinin AUMID'i düz `Chrome`, diğerleri `Chrome.UserData.ProfileN`.
- Başlatma hedefi pencerenin exe'si olamaz. Store uygulamalarının exe'si sürüm numaralı klasörde durur, doğrudan başlatılamaz ve güncellenince silinir; bunlar `shell:AppsFolder\<AUMID>` ile başlatılır (`launch::packaged_aumid`). Yardımcı süreçlerin (steamwebhelper) asıl uygulaması üst süreçtir (`tracker::find_launch`). Store exe'leri sürümden bağımsız karşılaştırılır (`apps::exe_key`). Steam'in RelaunchCommand'ı tırnaksız gelir; `split_command` `.exe`'den sonra böler.
- Kaynak hedefi: boşta CPU ~%0. Sürekli çizim döngüsü kurma; olay tabanlı çalış (WinEvent, WinRT olayları, seyrek zamanlayıcılar; dock gizliyken zamanlayıcılar iş yapmaz).
- Dock kapanırken ya da çökerken görev çubuğu **mutlaka** geri getirilmeli (panic hook + `cleanup`).

## Model iş bölümü ve maliyet

Ana oturum Opus ile çalışır. Hangi işi kimin yapacağı PLAN.md'deki adım tablosunda yazılı.

| Model | Ne yapar |
|---|---|
| **Opus** (ana oturum) | Tasarım, arayüz imzaları, `dock.rs` / `apps.rs` / `dock.slint` birleştirmesi, zamanlama ve geometri gibi hassas işler, inceleme, kullanıcıyla deneme |
| **Sonnet** (`Agent`, `model: "sonnet"`) | İmzası belli, tek dosyalık bağımsız modüller (Win32/WinRT sarmalayıcıları, veri modülleri, `.slint` ekranları) |
| **Haiku** (`model: "haiku"`) | SVG ikonlar, kodda arama/listeleme, mekanik düzenlemeler |

**Maliyet kuralları**
- **Her alt adım (5a, 5b…) ayrı sohbet.** Adım bitince PLAN.md tablosuna sonuç yaz, sohbeti orada bitir.
- **Otomatik devam (kullanıcı isteği, 2026-11-25):** adım bitince sıradaki adımı yeni bir oturumda kendin başlat (`start_session`, `context: "fresh"`, Opus). Brief kısa olsun: "PLAN.md'deki <adım>'ye başla; CLAUDE.md'deki kurallara uy." Şu iki durumda başlatma, kullanıcıyı bekle:
  - Adım kullanıcıdan bir karar istiyorsa (ör. 5a'nın pencere kararı).
  - PLAN.md'deki deneme sırası gereği kullanıcının dock'u denemesi gerekiyorsa. Deneme ve onay gelince sıradakini başlat.
  - Uygulama başlatmayı reddederse sıradaki adımı öneri kartı olarak bırak (`own_initiative`), kullanıcı tek tıkla başlatır.
- Agent talimatı tek başına yeterli olmalı; agent bu dosyayı ve konuşmayı görmez. Talimatta şunlar bulunur: dokunacağı dosya (sadece o), tam fonksiyon/struct imzaları, kullanılabilir crate özellikleri, kabul kriteri, "`cargo clippy` uyarısız", "Cargo.toml'a dokunma, gereken özelliği raporla", "kendi scratch crate'inde (`%TEMP%\claude\<ad>_scratch`, `#[path = ...] mod x;`) test et, proje kökünde derleme".
- Bağımsız agent'ları aynı mesajda paralel başlat. İki agent aynı dosyaya yazmaz. Agent çalışırken Opus başka dosyada çalışır.
- Gereken crate özelliğini agent'ı başlatmadan önce Cargo.toml'a ekle (agent'ın raporlamasını bekleme).
- Sonnet'in kodunu Opus kısaca okur, `cargo clippy` ile derler ve dener; ancak ondan sonra iş bitmiş sayılır.
- Uzun dosyaları bütün olarak okuma; Grep ile yeri bul, sadece o aralığı oku. Çok satırlı düzenlemelerde betik yerine Edit kullan.
