//! Top-down premultiplied 32-bit BGRA8 DIB surface and `UpdateLayeredWindowIndirect` presentation.

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{GetLastError, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ULW_ALPHA, UPDATELAYEREDWINDOWINFO, UpdateLayeredWindowIndirect,
};

use crate::RenderError;

/// Owns a memory DC and a 32-bit top-down BGRA8 DIB section.
///
/// The backing allocation is reused across frames whenever `(width, height)` is unchanged.
pub struct DibSurface {
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
    /// Number of old bitmaps that could not be deleted after a resize. Stays at 0 in
    /// healthy runs; exposed so tests can assert that resizing is GDI-neutral.
    delete_failures: u32,
}

impl DibSurface {
    pub fn new(width: u32, height: u32) -> Result<Self, RenderError> {
        let width = width.max(1);
        let height = height.max(1);

        let mem_dc = unsafe { CreateCompatibleDC(None) };
        if mem_dc.is_invalid() {
            return Err(RenderError::Win32(
                "CreateCompatibleDC returned an invalid DC".into(),
            ));
        }

        let (bitmap, bits) = match create_dib(mem_dc, width, height) {
            Ok(pair) => pair,
            Err(err) => {
                unsafe {
                    let _ = DeleteDC(mem_dc);
                }
                return Err(err);
            }
        };

        let old_bitmap = unsafe { SelectObject(mem_dc, HGDIOBJ(bitmap.0)) };

        Ok(Self {
            mem_dc,
            bitmap,
            old_bitmap,
            bits,
            width,
            height,
            delete_failures: 0,
        })
    }

    pub fn ensure_size(&mut self, width: u32, height: u32) -> Result<bool, RenderError> {
        let width = width.max(1);
        let height = height.max(1);
        if self.width == width && self.height == height {
            return Ok(false);
        }

        let (new_bitmap, new_bits) = create_dib(self.mem_dc, width, height)?;
        unsafe {
            // Deselect the current bitmap before deleting it: DeleteObject fails with
            // ERROR_BUSY while an HBITMAP is still selected into a DC, which would leak
            // both the GDI handle and the whole DIB allocation until process exit.
            let previous = SelectObject(self.mem_dc, HGDIOBJ(self.old_bitmap.0));
            let selected = SelectObject(self.mem_dc, HGDIOBJ(new_bitmap.0));
            // GDI documents both NULL and HGDI_ERROR (-1) as failure for SelectObject.
            if selected.is_invalid() || selected.0 as isize == -1 {
                // Restore the previous bitmap, drop the unused buffer and keep the old
                // surface intact so the caller can retry on the next frame.
                let _ = SelectObject(self.mem_dc, previous);
                let _ = DeleteObject(HGDIOBJ(new_bitmap.0));
                return Err(RenderError::win32(
                    "SelectObject(new DIB)",
                    "SelectObject returned HGDI_ERROR",
                ));
            }
            if !DeleteObject(HGDIOBJ(self.bitmap.0)).as_bool() {
                self.delete_failures = self.delete_failures.saturating_add(1);
                let error = GetLastError();
                tracing::warn!(
                    old_width = self.width,
                    old_height = self.height,
                    error = error.0,
                    "DeleteObject(old DIB) failed; the bitmap and its pixels stay allocated"
                );
            }
        }
        self.bitmap = new_bitmap;
        self.bits = new_bits;
        self.width = width;
        self.height = height;
        Ok(true)
    }

    /// Number of old bitmaps that leaked because `DeleteObject` failed. Expected to be 0.
    pub fn delete_failures(&self) -> u32 {
        self.delete_failures
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn hdc(&self) -> HDC {
        self.mem_dc
    }

    pub fn byte_len(&self) -> usize {
        (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4)
    }

    /// Raw premultiplied BGRA8 pixel buffer (top-down row order).
    pub fn pixels(&self) -> &[u8] {
        // SAFETY: `self.bits` was allocated by `CreateDIBSection` with `width * height * 4`
        // bytes and remains valid until `self.bitmap` is deleted in `Drop` or `ensure_size`.
        unsafe { std::slice::from_raw_parts(self.bits, self.byte_len()) }
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: exclusive `&mut self` guarantees unique access to the DIB buffer.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.byte_len()) }
    }

    pub fn clear(&mut self) {
        self.pixels_mut().fill(0);
    }
}

impl Drop for DibSurface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old_bitmap);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

fn create_dib(mem_dc: HDC, width: u32, height: u32) -> Result<(HBITMAP, *mut u8), RenderError> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative height creates a top-down DIB matching Direct2D coordinates.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: Default::default(),
    };

    let mut bits: *mut c_void = std::ptr::null_mut();
    let bitmap =
        unsafe { CreateDIBSection(Some(mem_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
            .map_err(|err| RenderError::win32("CreateDIBSection", err))?;

    if bitmap.is_invalid() || bits.is_null() {
        return Err(RenderError::Win32(
            "CreateDIBSection returned an invalid bitmap or null pixel pointer".into(),
        ));
    }
    Ok((bitmap, bits.cast::<u8>()))
}

/// Present a premultiplied BGRA8 `DibSurface` onto a `WS_EX_LAYERED` window via
/// `UpdateLayeredWindowIndirect` (per-pixel alpha, flicker-free).
pub fn present_layered_window(
    hwnd: HWND,
    surface: &DibSurface,
    position: Option<(i32, i32)>,
    global_alpha: u8,
) -> Result<(), RenderError> {
    let size = SIZE {
        cx: surface.width() as i32,
        cy: surface.height() as i32,
    };
    let src_pt = POINT { x: 0, y: 0 };
    let dst_pt = position.map(|(x, y)| POINT { x, y });
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: global_alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };

    let info = UPDATELAYEREDWINDOWINFO {
        cbSize: size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
        hdcDst: HDC::default(),
        pptDst: dst_pt
            .as_ref()
            .map(|p| p as *const POINT)
            .unwrap_or(std::ptr::null()),
        psize: &size,
        hdcSrc: surface.hdc(),
        pptSrc: &src_pt,
        crKey: windows::Win32::Foundation::COLORREF(0),
        pblend: &blend,
        dwFlags: ULW_ALPHA,
        prcDirty: std::ptr::null(),
    };

    let ok = unsafe { UpdateLayeredWindowIndirect(hwnd, &info) };
    if ok.as_bool() {
        Ok(())
    } else {
        let err = unsafe { GetLastError() };
        Err(RenderError::Win32(format!(
            "UpdateLayeredWindowIndirect failed: {}",
            err.0
        )))
    }
}

/// Convert premultiplied BGRA8 pixels to straight (or premultiplied) RGBA8 for
/// preview textures (e.g. `egui::ColorImage`).
pub fn bgra_premultiplied_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_surface_creates_clears_and_resizes() {
        let mut dib = DibSurface::new(64, 32).unwrap();
        assert_eq!(dib.width(), 64);
        assert_eq!(dib.height(), 32);
        assert_eq!(dib.pixels().len(), 64 * 32 * 4);

        dib.pixels_mut()[0] = 0xAA;
        dib.clear();
        assert_eq!(dib.pixels()[0], 0);

        assert!(!dib.ensure_size(64, 32).unwrap());
        assert!(dib.ensure_size(120, 40).unwrap());
        assert_eq!(dib.pixels().len(), 120 * 40 * 4);
    }

    #[test]
    fn bgra_swizzle_to_rgba() {
        let rgba = bgra_premultiplied_to_rgba(&[10, 20, 30, 255]);
        assert_eq!(rgba, vec![30, 20, 10, 255]);
    }
}
