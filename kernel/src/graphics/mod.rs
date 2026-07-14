pub mod console;
pub mod dirty_rect;

use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use dirty_rect::{DirtyTracker, Rect};
use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight, RasterizedChar};
use spin::Mutex;

#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const GREEN: Color = Color::rgb(80, 250, 123);
    pub const CYAN: Color = Color::rgb(139, 233, 253);
    pub const ORANGE: Color = Color::rgb(255, 184, 108);
    pub const BG: Color = Color::rgb(20, 22, 30);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    fn scaled(&self, intensity: u8) -> Color {
        let s = intensity as u16;
        Color::rgb(
            ((self.r as u16 * s) / 255) as u8,
            ((self.g as u16 * s) / 255) as u8,
            ((self.b as u16 * s) / 255) as u8,
        )
    }
}

pub const FONT_WEIGHT: FontWeight = FontWeight::Regular;
pub const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;
pub const CHAR_WIDTH: usize = get_raster_width(FONT_WEIGHT, FONT_HEIGHT);
pub const LINE_HEIGHT: usize = FONT_HEIGHT.val() + 2;

fn char_raster(c: char) -> RasterizedChar {
    get_raster(c, FONT_WEIGHT, FONT_HEIGHT)
        .unwrap_or_else(|| get_raster('?', FONT_WEIGHT, FONT_HEIGHT).expect("? must rasterize"))
}

/// All drawing targets `back`; `present()` copies only merged dirty rects
/// into the real (MMIO) framebuffer. Both buffers share the same byte layout
/// (stride x bytes_per_pixel) so a present is straight row memcpys.
pub struct Graphics {
    real_fb: &'static mut [u8],
    back: Vec<u8>,
    info: FrameBufferInfo,
    dirty: DirtyTracker,
}

impl Graphics {
    pub fn width(&self) -> usize {
        self.info.width
    }

    pub fn height(&self) -> usize {
        self.info.height
    }

    fn pixel_bytes(&self, color: Color) -> [u8; 4] {
        match self.info.pixel_format {
            PixelFormat::Rgb => [color.r, color.g, color.b, 0],
            PixelFormat::Bgr => [color.b, color.g, color.r, 0],
            PixelFormat::U8 => {
                let grey = ((color.r as u16 + color.g as u16 + color.b as u16) / 3) as u8;
                [grey, 0, 0, 0]
            }
            _ => [color.r, color.g, color.b, 0],
        }
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let bpp = self.info.bytes_per_pixel;
        let offset = (y * self.info.stride + x) * bpp;
        let bytes = self.pixel_bytes(color);
        self.back[offset..offset + bpp].copy_from_slice(&bytes[..bpp]);
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let rect = rect.clamp_to(self.info.width, self.info.height);
        let bpp = self.info.bytes_per_pixel;
        let bytes = self.pixel_bytes(color);
        for y in rect.y..rect.bottom() {
            let row_start = (y * self.info.stride + rect.x) * bpp;
            let row = &mut self.back[row_start..row_start + rect.w * bpp];
            for px in row.chunks_exact_mut(bpp) {
                px.copy_from_slice(&bytes[..bpp]);
            }
        }
        self.dirty.mark(rect);
    }

    /// Draws one glyph, returns its advance width. Background pixels are
    /// painted with `bg` (opaque console-style rendering).
    pub fn draw_char(&mut self, x: usize, y: usize, c: char, fg: Color, bg: Color) -> usize {
        let raster = char_raster(c);
        for (dy, row) in raster.raster().iter().enumerate() {
            for (dx, &intensity) in row.iter().enumerate() {
                let color = if intensity == 0 { bg } else { fg.scaled(intensity) };
                self.set_pixel(x + dx, y + dy, color);
            }
        }
        let w = raster.width();
        self.dirty.mark(Rect::new(x, y, w, raster.height()));
        w
    }

    pub fn draw_string(&mut self, x: usize, y: usize, s: &str, fg: Color, bg: Color) {
        let mut cx = x;
        for c in s.chars() {
            cx += self.draw_char(cx, y, c, fg, bg);
        }
    }

    #[allow(dead_code)] // used from Phase 5 on (cursor sprite drawn via set_pixel)
    pub fn mark_dirty(&mut self, rect: Rect) {
        self.dirty.mark(rect.clamp_to(self.info.width, self.info.height));
    }

    /// Copies all merged dirty rects from the back buffer to the real
    /// framebuffer, then clears the dirty list.
    pub fn present(&mut self) {
        let bpp = self.info.bytes_per_pixel;
        let (rects, n) = self.dirty.take();
        for rect in &rects[..n] {
            let rect = rect.clamp_to(self.info.width, self.info.height);
            for y in rect.y..rect.bottom() {
                let start = (y * self.info.stride + rect.x) * bpp;
                let len = rect.w * bpp;
                self.real_fb[start..start + len].copy_from_slice(&self.back[start..start + len]);
            }
        }
    }

    pub fn clear(&mut self, color: Color) {
        let (w, h) = (self.info.width, self.info.height);
        self.fill_rect(Rect::new(0, 0, w, h), color);
    }
}

pub static GRAPHICS: Mutex<Option<Graphics>> = Mutex::new(None);

pub fn init(real_fb: &'static mut [u8], info: FrameBufferInfo) {
    let back = vec![0u8; real_fb.len()];
    let mut g = Graphics {
        real_fb,
        back,
        info,
        dirty: DirtyTracker::new(),
    };
    g.clear(Color::BG);
    g.present();
    *GRAPHICS.lock() = Some(g);
}

pub fn with<R>(f: impl FnOnce(&mut Graphics) -> R) -> Option<R> {
    GRAPHICS.lock().as_mut().map(f)
}
