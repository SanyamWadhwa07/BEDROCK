//! Window manager: owns the window table + z-order, routes mouse/keyboard
//! input into drag/resize/focus/close, and compositess by redrawing only
//! the rects touched by this frame's events + animation (painter's
//! algorithm, back-to-front, within each dirty region) -- the same
//! dirty-rect discipline Phase 4 established, just driven by window events
//! instead of a single hardcoded sprite.

use super::window::{Window, WindowContent, MIN_HEIGHT, MIN_WIDTH, TITLEBAR_HEIGHT};
use super::zorder::ZOrder;
use crate::graphics::dirty_rect::Rect;
use crate::graphics::{self, Color};
use crate::input::mouse::PacketParser;
use crate::input::{KEYBOARD_QUEUE, MOUSE_QUEUE};
use crate::task::timer;
use alloc::string::String;
use alloc::vec::Vec;
use core::hint::spin_loop;
use pc_keyboard::{layouts, HandleControl, Keyboard, ScancodeSet1};

const RENDER_EVERY_TICKS: u64 = 3; // 100Hz PIT / 3 ~= 33fps target
const CURSOR_SIZE: usize = 13;

enum Drag {
    Move { window: usize, grab_dx: i32, grab_dy: i32 },
    Resize { window: usize },
}

struct WindowManager {
    windows: Vec<Window>,
    order: ZOrder,
    focused: Option<usize>,
    drag: Option<Drag>,
    next_id: u32,
}

impl WindowManager {
    fn new() -> Self {
        Self {
            windows: Vec::new(),
            order: ZOrder::new(),
            focused: None,
            drag: None,
            next_id: 0,
        }
    }

    fn spawn_window(&mut self, title: &str, rect: Rect, content: WindowContent) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.windows.push(Window {
            id,
            rect,
            title: String::from(title),
            content,
        });
        let idx = self.windows.len() - 1;
        self.order.push_top(idx);
        self.focused = Some(idx);
        idx
    }

    /// Removes the window and fixes up z-order indices + focus; returns
    /// the screen rect it used to occupy so the caller can repaint it.
    fn close_window(&mut self, idx: usize) -> Rect {
        let rect = self.windows[idx].rect;
        self.windows.remove(idx);
        self.order.remove(idx);
        self.focused = match self.focused {
            Some(f) if f == idx => self.order.top(),
            Some(f) if f > idx => Some(f - 1),
            other => other,
        };
        rect
    }

    fn focus(&mut self, idx: usize) {
        self.order.raise(idx);
        self.focused = Some(idx);
    }
}

pub extern "C" fn wm_task() -> ! {
    let (screen_w, screen_h) =
        graphics::with(|g| (g.width(), g.height())).expect("graphics not initialized");

    // Clear the boot console text; from here on the WM owns the screen.
    graphics::with(|g| {
        g.clear(Color::BG);
        g.present();
    });

    let mut wm = WindowManager::new();
    wm.spawn_window(
        "Stats",
        Rect::new(60, 50, 260, 150),
        WindowContent::Stats { box_x: 4, box_y: 34, vx: 3, vy: 2 },
    );
    wm.spawn_window("Terminal", Rect::new(360, 40, 340, 160), WindowContent::new_terminal());
    wm.spawn_window("Editor", Rect::new(360, 230, 340, 180), WindowContent::new_editor());

    let mut keyboard = Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore);
    let mut mouse_parser = PacketParser::new();
    let mut cursor_x: i32 = (screen_w / 2) as i32;
    let mut cursor_y: i32 = (screen_h / 2) as i32;
    let mut prev_cursor = Rect::new(cursor_x as usize, cursor_y as usize, CURSOR_SIZE, CURSOR_SIZE);
    let mut buttons: u8 = 0;
    let mut last_render_tick = 0u64;

    loop {
        let mut dirty: Vec<Rect> = Vec::new();

        // --- keyboard: routed only to the focused window ---
        while let Some(scancode) = KEYBOARD_QUEUE.pop() {
            if let Ok(Some(event)) = keyboard.add_byte(scancode) {
                if let Some(key) = keyboard.process_keyevent(event) {
                    if let Some(fidx) = wm.focused {
                        if let Some(rect) = wm.windows[fidx].handle_key(key) {
                            dirty.push(rect);
                        }
                    }
                }
            }
        }

        // --- mouse: hit-test, drag/resize, focus/raise, close ---
        while let Some(byte) = MOUSE_QUEUE.pop() {
            if let Some(ev) = mouse_parser.feed(byte) {
                cursor_x = (cursor_x + ev.dx as i32).clamp(0, screen_w as i32 - 1);
                cursor_y = (cursor_y + ev.dy as i32).clamp(0, screen_h as i32 - 1);
                let cx = cursor_x as usize;
                let cy = cursor_y as usize;
                let pressed_now = ev.buttons & 1 != 0;
                let pressed_before = buttons & 1 != 0;

                if let Some(d) = &wm.drag {
                    match *d {
                        Drag::Move { window, grab_dx, grab_dy } => {
                            let old = wm.windows[window].rect;
                            let win_w = old.w;
                            let nx = (cursor_x - grab_dx).clamp(0, (screen_w as i32 - win_w as i32).max(0));
                            let ny =
                                (cursor_y - grab_dy).clamp(0, (screen_h as i32 - TITLEBAR_HEIGHT as i32).max(0));
                            wm.windows[window].rect.x = nx as usize;
                            wm.windows[window].rect.y = ny as usize;
                            dirty.push(old.union(&wm.windows[window].rect));
                        }
                        Drag::Resize { window } => {
                            let old = wm.windows[window].rect;
                            let new_w = ((cx as i32 - old.x as i32).max(MIN_WIDTH as i32)) as usize;
                            let new_h = ((cy as i32 - old.y as i32).max(MIN_HEIGHT as i32)) as usize;
                            let win = &mut wm.windows[window];
                            win.rect.w = new_w.min(screen_w.saturating_sub(win.rect.x));
                            win.rect.h = new_h.min(screen_h.saturating_sub(win.rect.y));
                            dirty.push(old.union(&win.rect));
                        }
                    }
                    if !pressed_now {
                        wm.drag = None;
                    }
                } else if pressed_now && !pressed_before {
                    if let Some(idx) = wm.order.hit_test(&wm.windows, cx, cy) {
                        let close_rect = wm.windows[idx].close_button_rect();
                        let resize_rect = wm.windows[idx].resize_handle_rect();
                        let title_rect = wm.windows[idx].titlebar_rect();
                        if close_rect.contains(cx, cy) {
                            dirty.push(wm.close_window(idx));
                        } else if resize_rect.contains(cx, cy) {
                            wm.focus(idx);
                            wm.drag = Some(Drag::Resize { window: idx });
                            dirty.push(wm.windows[idx].rect);
                        } else if title_rect.contains(cx, cy) {
                            let win_rect = wm.windows[idx].rect;
                            let grab_dx = cursor_x - win_rect.x as i32;
                            let grab_dy = cursor_y - win_rect.y as i32;
                            wm.focus(idx);
                            wm.drag = Some(Drag::Move { window: idx, grab_dx, grab_dy });
                            dirty.push(win_rect);
                        } else {
                            wm.focus(idx);
                            dirty.push(wm.windows[idx].rect);
                        }
                    }
                }
                buttons = ev.buttons;
            }
        }

        let now = timer::ticks();
        if now < last_render_tick + RENDER_EVERY_TICKS {
            spin_loop();
            continue;
        }
        last_render_tick = now;

        for win in wm.windows.iter_mut() {
            if let Some(r) = win.step_animation() {
                dirty.push(r);
            }
        }

        // Cursor is redrawn every frame (cheap: two thin bars) so it never
        // leaves a trail regardless of what else did or didn't move.
        let new_cursor = Rect::new(cursor_x.max(0) as usize, cursor_y.max(0) as usize, CURSOR_SIZE, CURSOR_SIZE);
        dirty.push(prev_cursor.union(&new_cursor));

        if !dirty.is_empty() {
            graphics::with(|g| {
                for region in &dirty {
                    let region = region.clamp_to(screen_w, screen_h);
                    g.fill_rect(region, Color::BG);
                    for idx in wm.order.back_to_front() {
                        let win = &wm.windows[idx];
                        if win.rect.touches(&region) {
                            win.draw(g, wm.focused == Some(idx));
                        }
                    }
                }
                prev_cursor = draw_cursor(g, cursor_x.max(0) as usize, cursor_y.max(0) as usize);
                g.present();
            });
        }
    }
}

/// Crosshair cursor: one horizontal + one vertical bar, always drawn last
/// so it sits on top of every window. Returns the rect it occupied so the
/// next frame can erase it.
fn draw_cursor(g: &mut graphics::Graphics, cx: usize, cy: usize) -> Rect {
    let half = CURSOR_SIZE / 2;
    let x0 = cx.saturating_sub(half);
    let y0 = cy.saturating_sub(half);
    g.fill_rect(Rect::new(x0, cy.saturating_sub(1), CURSOR_SIZE, 3), Color::WHITE);
    g.fill_rect(Rect::new(cx.saturating_sub(1), y0, 3, CURSOR_SIZE), Color::WHITE);
    Rect::new(x0, y0, CURSOR_SIZE, CURSOR_SIZE)
}
