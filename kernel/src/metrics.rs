//! Live GUI/compositor metrics, published by the window-manager task as
//! plain atomics so the Stats window and the serial `metrics` command can
//! read them without any lock the render loop might be holding.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Frames actually presented (composite + present ran), monotonic.
pub static FRAMES_PRESENTED: AtomicU64 = AtomicU64::new(0);
/// Frames per second, recomputed by the WM once per second.
pub static FPS: AtomicU64 = AtomicU64::new(0);
/// Composite+present time of the most recent frame, in microseconds.
pub static FRAME_TIME_US_LAST: AtomicU64 = AtomicU64::new(0);
/// Worst composite+present time observed in the current one-second window
/// (published to `FRAME_TIME_US_MAX` when the window rolls over).
pub static FRAME_TIME_US_WINDOW_MAX: AtomicU64 = AtomicU64::new(0);
/// Worst composite+present time of the *previous* one-second window.
pub static FRAME_TIME_US_MAX: AtomicU64 = AtomicU64::new(0);

/// TSC stamp taken in the IRQ12 handler for the most recent mouse byte;
/// the WM uses the stamp of the byte that *completed* a packet.
pub static MOUSE_ISR_TSC: AtomicU64 = AtomicU64::new(0);
/// Mouse-input latency (IRQ handler -> pixels presented), microseconds,
/// for the most recent frame that included cursor movement.
pub static INPUT_LATENCY_US_LAST: AtomicU64 = AtomicU64::new(0);
/// Exponential moving average (alpha = 1/8) of the above.
pub static INPUT_LATENCY_US_EMA: AtomicU64 = AtomicU64::new(0);

/// When set, the WM marks the whole screen dirty every frame -- the "naive
/// full redraw" strawman, toggleable at runtime (`redraw naive|dirty` on the
/// serial shell) so dirty-rect compositing has a measured baseline to beat.
pub static FORCE_FULL_REDRAW: AtomicBool = AtomicBool::new(false);

pub fn record_input_latency(us: u64) {
    INPUT_LATENCY_US_LAST.store(us, Ordering::Relaxed);
    let ema = INPUT_LATENCY_US_EMA.load(Ordering::Relaxed);
    let new_ema = if ema == 0 { us } else { ema - ema / 8 + us / 8 };
    INPUT_LATENCY_US_EMA.store(new_ema, Ordering::Relaxed);
}

pub fn record_frame_time(us: u64) {
    FRAME_TIME_US_LAST.store(us, Ordering::Relaxed);
    FRAME_TIME_US_WINDOW_MAX.fetch_max(us, Ordering::Relaxed);
}

/// Called by the WM when its one-second FPS window rolls over.
pub fn roll_fps_window(frames_in_window: u64) {
    FPS.store(frames_in_window, Ordering::Relaxed);
    FRAME_TIME_US_MAX.store(
        FRAME_TIME_US_WINDOW_MAX.swap(0, Ordering::Relaxed),
        Ordering::Relaxed,
    );
}
