//! Phase-4/5 render + input task: a bouncing rectangle, live FPS counter,
//! background-task counters, a mouse-driven cursor sprite, and a typed-text
//! echo line. All input decoding happens here in task context (the ISRs
//! only push raw bytes into the SPSC rings), and all drawing goes through
//! the back buffer + merged dirty rects.

use crate::graphics::dirty_rect::Rect;
use crate::graphics::{self, Color, LINE_HEIGHT};
use crate::input::mouse::PacketParser;
use crate::input::{KEYBOARD_QUEUE, MOUSE_QUEUE};
use crate::serial_println;
use crate::task::timer;
use alloc::format;
use alloc::string::String;
use core::hint::spin_loop;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

const RENDER_EVERY_TICKS: u64 = 3; // 100Hz PIT / 3 ~= 33 fps target
const BOX_SIZE: usize = 48;
const HUD_LINES: usize = 4;
const CURSOR_SIZE: usize = 13;
const TYPED_CAP: usize = 60;

pub extern "C" fn render_task() -> ! {
    let (screen_w, screen_h) =
        graphics::with(|g| (g.width(), g.height())).expect("graphics not initialized");

    // Clear the boot console text; from here on the render task owns the screen.
    graphics::with(|g| {
        g.clear(Color::BG);
        g.present();
    });

    let hud_height = HUD_LINES * LINE_HEIGHT + 8;
    let mut x: i32 = 60;
    let mut y: i32 = hud_height as i32 + 20;
    let mut vx: i32 = 4;
    let mut vy: i32 = 3;

    let mut keyboard = Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore);
    let mut mouse_parser = PacketParser::new();
    let mut cursor_x: i32 = (screen_w / 2) as i32;
    let mut cursor_y: i32 = (screen_h / 2) as i32;
    let mut prev_cursor = Rect::new(cursor_x as usize, cursor_y as usize, CURSOR_SIZE, CURSOR_SIZE);
    let mut buttons: u8 = 0;
    let mut typed = String::new();

    let mut last_render_tick = 0u64;
    let mut frames_this_second = 0u32;
    let mut fps = 0u32;
    let mut last_fps_tick = 0u64;

    loop {
        // --- Drain input queues (decode in task context, never in the ISR) ---
        while let Some(scancode) = KEYBOARD_QUEUE.pop() {
            if let Ok(Some(event)) = keyboard.add_byte(scancode) {
                if let Some(key) = keyboard.process_keyevent(event) {
                    match key {
                        DecodedKey::Unicode(c) => {
                            serial_println!("kbd: {:?}", c);
                            match c {
                                '\u{8}' => {
                                    typed.pop();
                                }
                                '\r' | '\n' => typed.clear(),
                                c if !c.is_control() => {
                                    if typed.len() >= TYPED_CAP {
                                        typed.clear();
                                    }
                                    typed.push(c);
                                }
                                _ => {}
                            }
                        }
                        DecodedKey::RawKey(k) => serial_println!("kbd raw: {:?}", k),
                    }
                }
            }
        }
        while let Some(byte) = MOUSE_QUEUE.pop() {
            if let Some(ev) = mouse_parser.feed(byte) {
                cursor_x = (cursor_x + ev.dx as i32).clamp(0, screen_w as i32 - 1);
                cursor_y = (cursor_y + ev.dy as i32).clamp(0, screen_h as i32 - 1);
                if ev.buttons != buttons {
                    serial_println!("mouse buttons: {:03b}", ev.buttons);
                }
                buttons = ev.buttons;
                serial_println!("mouse: dx={} dy={} -> ({}, {})", ev.dx, ev.dy, cursor_x, cursor_y);
            }
        }

        let now = timer::ticks();
        if now < last_render_tick + RENDER_EVERY_TICKS {
            spin_loop();
            continue;
        }
        last_render_tick = now;

        // FPS bookkeeping: PIT runs at 100Hz, so 100 ticks ~= 1 second.
        frames_this_second += 1;
        if now >= last_fps_tick + 100 {
            fps = frames_this_second;
            frames_this_second = 0;
            last_fps_tick = now;
        }

        // Physics: bounce off screen edges (staying below the HUD).
        let old_box = Rect::new(x as usize, y as usize, BOX_SIZE, BOX_SIZE);
        x += vx;
        y += vy;
        if x <= 0 || x as usize + BOX_SIZE >= screen_w {
            vx = -vx;
            x = x.clamp(0, (screen_w - BOX_SIZE) as i32);
        }
        if y <= hud_height as i32 || y as usize + BOX_SIZE >= screen_h {
            vy = -vy;
            y = y.clamp(hud_height as i32, (screen_h - BOX_SIZE) as i32);
        }
        let new_box = Rect::new(x as usize, y as usize, BOX_SIZE, BOX_SIZE);

        let counters = crate::demo_tasks::snapshot();
        let line1 = format!("Bedrock  |  Phase 5: graphics + input  |  {} fps", fps);
        let line2 = format!(
            "tasks: {}={} {}={} {}={} {}={} {}={}",
            counters[0].0, counters[0].1,
            counters[1].0, counters[1].1,
            counters[2].0, counters[2].1,
            counters[3].0, counters[3].1,
            counters[4].0, counters[4].1,
        );
        let line3 = format!(
            "uptime: {}s  |  cursor: ({}, {})  buttons: {:03b}",
            now / 100, cursor_x, cursor_y, buttons
        );
        let line4 = format!("> {}", typed);

        graphics::with(|g| {
            // Erase old box + old cursor, draw new box.
            g.fill_rect(old_box, Color::BG);
            g.fill_rect(prev_cursor, Color::BG);
            g.fill_rect(new_box, Color::ORANGE);
            // HUD strip at the top (fill_rect marks it dirty itself).
            g.fill_rect(Rect::new(0, 0, screen_w, hud_height), Color::BG);
            g.draw_string(8, 4, &line1, Color::CYAN, Color::BG);
            g.draw_string(8, 4 + LINE_HEIGHT, &line2, Color::GREEN, Color::BG);
            g.draw_string(8, 4 + 2 * LINE_HEIGHT, &line3, Color::WHITE, Color::BG);
            g.draw_string(8, 4 + 3 * LINE_HEIGHT, &line4, Color::ORANGE, Color::BG);
            // Cursor sprite last so it sits on top of everything.
            prev_cursor = draw_cursor(g, cursor_x as usize, cursor_y as usize);
            g.present();
        });
    }
}

/// Crosshair cursor: one horizontal + one vertical bar. Returns the rect it
/// occupied so the next frame can erase it.
fn draw_cursor(g: &mut graphics::Graphics, cx: usize, cy: usize) -> Rect {
    let half = CURSOR_SIZE / 2;
    let x0 = cx.saturating_sub(half);
    let y0 = cy.saturating_sub(half);
    g.fill_rect(Rect::new(x0, cy.saturating_sub(1), CURSOR_SIZE, 3), Color::WHITE);
    g.fill_rect(Rect::new(cx.saturating_sub(1), y0, 3, CURSOR_SIZE), Color::WHITE);
    Rect::new(x0, y0, CURSOR_SIZE, CURSOR_SIZE)
}
