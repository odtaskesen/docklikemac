//! Kabuk üzerinden yüksek kaliteli ikon çıkarma (IShellItemImageFactory).

use windows::core::PCWSTR;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
};

/// Düz (premultiplied olmayan) RGBA8, satır sıralı, yukarıdan aşağıya. len == width*height*4.
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Bu thread'de COM'u (apartment) başlatır; tekrar çağrılabilir.
pub fn com_init_thread() {
    // Sonuç bilinçli yok sayılır: S_FALSE (zaten başlatılmış) ve RPC_E_CHANGED_MODE zararsız.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// HBITMAP'i serbest bırakan küçük koruyucu.
struct BitmapGuard(HBITMAP);
impl Drop for BitmapGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}

/// `parsing_name` için `size`x`size` ikon yükler (yol veya "shell:AppsFolder\<AUMID>").
pub fn load_icon(parsing_name: &str, size: u32) -> Option<IconPixels> {
    if size == 0 || size > 1024 {
        return None;
    }
    let wide: Vec<u16> = parsing_name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
        let hbmp = factory
            .GetImage(SIZE { cx: size as i32, cy: size as i32 }, SIIGBF_ICONONLY)
            .ok()?;
        let _guard = BitmapGuard(hbmp);
        bitmap_to_rgba(hbmp)
    }
}

/// HBITMAP -> straight RGBA.
unsafe fn bitmap_to_rgba(hbmp: HBITMAP) -> Option<IconPixels> {
    let mut bm = BITMAP::default();
    let got = GetObjectW(
        HGDIOBJ(hbmp.0),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut BITMAP as *mut _),
    );
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return None;
    }
    let width = bm.bmWidth as u32;
    let height = bm.bmHeight.unsigned_abs();

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // yukarıdan aşağıya
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut buf = vec![0u8; (width * height * 4) as usize];
    let hdc = CreateCompatibleDC(None);
    if hdc.is_invalid() {
        return None;
    }
    let lines = GetDIBits(
        hdc,
        hbmp,
        0,
        height,
        Some(buf.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );
    let _ = DeleteDC(hdc);
    if lines == 0 {
        return None;
    }

    // Tüm alfa sıfırsa bitmap opak kabul edilir.
    let all_zero_alpha = buf.iter().skip(3).step_by(4).all(|&a| a == 0);
    for px in buf.chunks_mut(4) {
        let (b, g, r) = (px[0], px[1], px[2]);
        let a = if all_zero_alpha { 255 } else { px[3] };
        let unpremul = |c: u8| -> u8 {
            if a == 255 || a == 0 {
                c
            } else {
                ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
            }
        };
        px[0] = unpremul(r);
        px[1] = unpremul(g);
        px[2] = unpremul(b);
        px[3] = a;
    }
    Some(IconPixels { width, height, rgba: buf })
}

