//! Boot-time text console drawn through the graphics back buffer. Only used
//! before the render task takes over the screen; each print presents
//! immediately so text is visible without a running render loop.

use super::{Color, LINE_HEIGHT};
use core::fmt;
use spin::Mutex;

const BORDER: usize = 4;

struct ConsoleState {
    x: usize,
    y: usize,
}

static CONSOLE: Mutex<ConsoleState> = Mutex::new(ConsoleState { x: BORDER, y: BORDER });

struct ConsoleWriter;

impl fmt::Write for ConsoleWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut state = CONSOLE.lock();
        super::with(|g| {
            for c in s.chars() {
                match c {
                    '\n' => {
                        state.x = BORDER;
                        state.y += LINE_HEIGHT;
                    }
                    '\r' => state.x = BORDER,
                    c => {
                        if state.x + super::CHAR_WIDTH >= g.width() {
                            state.x = BORDER;
                            state.y += LINE_HEIGHT;
                        }
                        if state.y + LINE_HEIGHT >= g.height() {
                            g.clear(Color::BG);
                            state.x = BORDER;
                            state.y = BORDER;
                        }
                        state.x += g.draw_char(state.x, state.y, c, Color::GREEN, Color::BG);
                    }
                }
            }
            g.present();
        });
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    ConsoleWriter.write_fmt(args).ok();
}

#[macro_export]
macro_rules! fb_print {
    ($($arg:tt)*) => {
        $crate::graphics::console::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! fb_println {
    () => ($crate::fb_print!("\n"));
    ($fmt:expr) => ($crate::fb_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => (
        $crate::fb_print!(concat!($fmt, "\n"), $($arg)*)
    );
}
