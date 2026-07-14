//! A window is a titled, draggable/resizable rectangle on the desktop.
//! Content is drawn immediate-mode -- no retained widget tree -- by
//! whichever `WindowContent` variant it holds; `draw()` is the only thing
//! that touches the screen, `step_animation()` is the only thing that
//! mutates content state on its own (independent of input events).

use crate::demo_tasks;
use crate::graphics::dirty_rect::Rect;
use crate::graphics::{self, Color, LINE_HEIGHT};
use alloc::format;
use alloc::string::String;

pub const TITLEBAR_HEIGHT: usize = 20;
pub const CLOSE_BTN_SIZE: usize = 14;
pub const RESIZE_HANDLE: usize = 10;
pub const MIN_WIDTH: usize = 150;
pub const MIN_HEIGHT: usize = 90;

const BOX_SIZE: i32 = 18;
const TOP_MARGIN: i32 = 34; // room for two HUD text lines above the bouncing box

pub type WindowId = u32;

pub enum WindowContent {
    /// Live scheduler counters + a bouncing box, bounced off this window's
    /// *own* content bounds (not the screen) -- proves per-window state
    /// survives drag/resize independently of every other window.
    Stats { box_x: i32, box_y: i32, vx: i32, vy: i32 },
    /// Typed-text echo; only receives keystrokes while focused, the
    /// smallest possible demonstration of focus-routed input.
    Console { typed: String },
}

pub struct Window {
    #[allow(dead_code)] // identity handle for future multi-window IPC/focus APIs
    pub id: WindowId,
    pub rect: Rect,
    pub title: String,
    pub content: WindowContent,
}

impl Window {
    pub fn titlebar_rect(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, TITLEBAR_HEIGHT)
    }

    pub fn close_button_rect(&self) -> Rect {
        let pad = 3;
        Rect::new(
            self.rect.right().saturating_sub(CLOSE_BTN_SIZE + pad),
            self.rect.y + pad,
            CLOSE_BTN_SIZE,
            CLOSE_BTN_SIZE,
        )
    }

    pub fn resize_handle_rect(&self) -> Rect {
        Rect::new(
            self.rect.right().saturating_sub(RESIZE_HANDLE),
            self.rect.bottom().saturating_sub(RESIZE_HANDLE),
            RESIZE_HANDLE,
            RESIZE_HANDLE,
        )
    }

    pub fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x + 1,
            self.rect.y + TITLEBAR_HEIGHT,
            self.rect.w.saturating_sub(2),
            self.rect.h.saturating_sub(TITLEBAR_HEIGHT + 1),
        )
    }

    /// Steps this window's own animation (if it has one), returning the
    /// screen-space rect that needs repainting because of it -- the
    /// bounding-box union of the box's old and new position, same merge
    /// primitive the dirty-rect tracker uses.
    pub fn step_animation(&mut self) -> Option<Rect> {
        let cr = self.content_rect();
        let WindowContent::Stats { box_x, box_y, vx, vy } = &mut self.content else {
            return None;
        };
        let max_x = (cr.w as i32 - BOX_SIZE - 2).max(1);
        let max_y = (cr.h as i32 - BOX_SIZE - 2).max(TOP_MARGIN + 1);

        let old = Rect::new(
            (cr.x as i32 + *box_x).max(0) as usize,
            (cr.y as i32 + *box_y).max(0) as usize,
            BOX_SIZE as usize,
            BOX_SIZE as usize,
        );

        *box_x += *vx;
        *box_y += *vy;
        if *box_x <= 0 || *box_x >= max_x {
            *vx = -*vx;
            *box_x = (*box_x).clamp(0, max_x);
        }
        if *box_y <= TOP_MARGIN || *box_y >= max_y {
            *vy = -*vy;
            *box_y = (*box_y).clamp(TOP_MARGIN, max_y);
        }

        let new = Rect::new(
            (cr.x as i32 + *box_x).max(0) as usize,
            (cr.y as i32 + *box_y).max(0) as usize,
            BOX_SIZE as usize,
            BOX_SIZE as usize,
        );
        Some(old.union(&new))
    }

    pub fn draw(&self, g: &mut graphics::Graphics, focused: bool) {
        let border = if focused { Color::CYAN } else { Color::rgb(80, 84, 100) };
        let titlebar_bg = if focused { Color::rgb(40, 70, 90) } else { Color::rgb(35, 37, 46) };

        // Border, then an inset fill, gives a 1px frame without a dedicated
        // stroke primitive.
        g.fill_rect(self.rect, border);
        let inner = Rect::new(
            self.rect.x + 1,
            self.rect.y + 1,
            self.rect.w.saturating_sub(2),
            self.rect.h.saturating_sub(2),
        );
        g.fill_rect(inner, Color::BG);

        g.fill_rect(self.titlebar_rect(), titlebar_bg);
        g.draw_string(self.rect.x + 6, self.rect.y + 3, &self.title, Color::WHITE, titlebar_bg);

        let cb = self.close_button_rect();
        g.fill_rect(cb, Color::rgb(180, 60, 60));
        g.draw_string(cb.x + 3, cb.y, "x", Color::WHITE, Color::rgb(180, 60, 60));

        let rh = self.resize_handle_rect();
        g.fill_rect(rh, Color::rgb(60, 63, 76));

        self.draw_content(g, focused);
    }

    fn draw_content(&self, g: &mut graphics::Graphics, focused: bool) {
        let cr = self.content_rect();
        match &self.content {
            WindowContent::Stats { box_x, box_y, .. } => {
                let counters = demo_tasks::snapshot();
                let l1 = format!("{}={} {}={}", counters[0].0, counters[0].1, counters[1].0, counters[1].1);
                let l2 = format!(
                    "{}={} {}={} {}={}",
                    counters[2].0, counters[2].1, counters[3].0, counters[3].1, counters[4].0, counters[4].1
                );
                g.draw_string(cr.x + 4, cr.y + 4, &l1, Color::GREEN, Color::BG);
                g.draw_string(cr.x + 4, cr.y + 4 + LINE_HEIGHT, &l2, Color::GREEN, Color::BG);
                let bx = (cr.x as i32 + *box_x).max(0) as usize;
                let by = (cr.y as i32 + *box_y).max(0) as usize;
                g.fill_rect(Rect::new(bx, by, BOX_SIZE as usize, BOX_SIZE as usize), Color::ORANGE);
            }
            WindowContent::Console { typed } => {
                let prefix = if focused { "> " } else { "  " };
                let line = format!("{}{}", prefix, typed);
                g.draw_string(cr.x + 4, cr.y + 4, &line, Color::CYAN, Color::BG);
            }
        }
    }
}
