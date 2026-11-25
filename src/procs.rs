//! Çalışan süreçlerin exe yolları.
use std::collections::{HashMap, HashSet};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Yalnızca taban dosya adı `names` içinde olan süreçlerin tam yolları (küçük harf) → bir pid.
/// `names`: "steam.exe" gibi küçük harfli dosya adları. Diğer süreçler için
/// OpenProcess çağrılmaz, bu yüzden sık çağrılan yol budur.
pub fn running_exes_matching(names: &HashSet<String>) -> HashMap<String, u32> {
    collect(|base| names.contains(base))
}

/// Süreç kimliği → üst süreç kimliği.
pub fn parent_pids() -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    walk(|e| {
        out.insert(e.th32ProcessID, e.th32ParentProcessID);
    });
    out
}

/// Toolhelp anlık görüntüsündeki her süreç için `f`'yi çağırır.
fn walk(mut f: impl FnMut(&PROCESSENTRY32W)) {
    // SAFETY: Geçerli bayrakla anlık görüntü alınır; tutamak aşağıda kapatılır.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return;
    }
    // SAFETY: PROCESSENTRY32W düz veri yapısı, sıfırlanması geçerli.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: snap geçerli, entry.dwSize ayarlı.
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) } != 0;
    while ok {
        f(&entry);
        // SAFETY: snap geçerli, entry geçerli ve dwSize ayarlı.
        ok = unsafe { Process32NextW(snap, &mut entry) } != 0;
    }
    // SAFETY: snap bu fonksiyonda açıldı ve burada bir kez kapatılıyor.
    unsafe { CloseHandle(snap) };
}

/// Toolhelp anlık görüntüsünü gezer; `want(küçük harfli taban ad)` true ise yolu alır.
fn collect(want: impl Fn(&str) -> bool) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    let mut buf = [0u16; 1024];
    walk(|entry| {
        let pid = entry.th32ProcessID;
        if pid == 0 || pid == 4 {
            return;
        }
        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        let base = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
        if want(&base) {
            if let Some(path) = image_path(pid, &mut buf) {
                out.insert(path, pid);
            }
        }
    });
    out
}

/// Sürecin tam exe yolunu küçük harfle döndürür; açılamazsa None.
pub fn image_path(pid: u32, buf: &mut [u16]) -> Option<String> {
    // SAFETY: Salt sorgu erişimiyle açılır; tutamak aşağıda kapatılır.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut len = buf.len() as u32;
    // SAFETY: h geçerli; buf, len kadar u16 yazılabilir alan sağlar.
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) } != 0;
    // SAFETY: h yukarıda açıldı, bir kez kapatılıyor.
    unsafe { CloseHandle(h) };
    if ok && len > 0 {
        Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
    } else {
        None
    }
}
