use crate::{serial_print, serial_println};
use alloc::boxed::Box;
use alloc::vec::Vec;
use spin::Mutex;

pub struct MemSummary {
    pub usable_bytes: u64,
    pub region_count: usize,
}

pub static MEM_INFO: Mutex<Option<MemSummary>> = Mutex::new(None);

pub fn set_mem_info(usable_bytes: u64, region_count: usize) {
    *MEM_INFO.lock() = Some(MemSummary {
        usable_bytes,
        region_count,
    });
}

const LINE_CAP: usize = 128;

pub fn run() -> ! {
    let mut line = [0u8; LINE_CAP];
    let mut len = 0usize;

    serial_println!();
    print_prompt();

    loop {
        // Bytes arrive via the IRQ4 handler into a lock-free ring buffer.
        // The shell never touches the UART's receive path itself: as one of
        // several round-robin tasks it is descheduled far too often to poll
        // a one-byte register without losing input.
        let Some(byte) = crate::serial::SERIAL_QUEUE.pop() else {
            core::hint::spin_loop();
            continue;
        };
        match byte {
            b'\r' | b'\n' => {
                serial_println!();
                let cmd = core::str::from_utf8(&line[..len]).unwrap_or("");
                dispatch(cmd);
                len = 0;
                print_prompt();
            }
            0x08 | 0x7f => {
                // backspace / DEL
                if len > 0 {
                    len -= 1;
                    serial_print!("\u{8} \u{8}");
                }
            }
            byte if byte.is_ascii_graphic() || byte == b' ' => {
                if len < LINE_CAP {
                    line[len] = byte;
                    len += 1;
                    serial_print!("{}", byte as char);
                }
            }
            _ => {}
        }
    }
}

fn print_prompt() {
    serial_print!("bedrock> ");
}

fn dispatch(cmd: &str) {
    let cmd = cmd.trim();
    let mut parts = cmd.splitn(2, ' ');
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match name {
        "" => {}
        "help" => {
            serial_println!(
                "commands: help, echo <text>, meminfo, frameinfo, alloctest, tasks, uptime"
            );
            serial_println!(
                "          bench <ata|alloc|fs|ctxswitch|all>, metrics, fscache <on|off>"
            );
            serial_println!("          redraw <naive|dirty>, fault <breakpoint|page|gpf|double>");
        }
        "alloctest" => cmd_alloctest(),
        "bench" => crate::bench::run(arg),
        "metrics" => cmd_metrics(),
        "fscache" => cmd_fscache(arg),
        "redraw" => cmd_redraw(arg),
        "tasks" => cmd_tasks(),
        "uptime" => {
            let ms = crate::task::timer::uptime_ms();
            serial_println!(
                "up {}.{:02}s ({} timer ticks)",
                ms / 1000,
                (ms % 1000) / 10,
                crate::task::timer::ticks()
            );
        }
        "echo" => serial_println!("{}", arg),
        "meminfo" => match MEM_INFO.lock().as_ref() {
            Some(info) => serial_println!(
                "usable memory: {} bytes ({} MiB) across {} regions",
                info.usable_bytes,
                info.usable_bytes / (1024 * 1024),
                info.region_count
            ),
            None => serial_println!("meminfo not available"),
        },
        "frameinfo" => serial_println!(
            "frames: {} free / {} total",
            crate::memory::frame_allocator::free_frame_count(),
            crate::memory::frame_allocator::total_frame_count()
        ),
        "fault" => cmd_fault(arg),
        other => serial_println!("unknown command: {} (try 'help')", other),
    }
}

fn cmd_tasks() {
    let stats = crate::task::scheduler::task_stats();
    let total_ticks: u64 = stats.iter().map(|t| t.ticks_run).sum::<u64>().max(1);
    serial_println!("{} tasks (cpu% = ticks_run share since boot):", stats.len());
    for t in &stats {
        serial_println!(
            "  task {}: {:?}, {} switches, {} ticks run (~{}%)",
            t.id,
            t.state,
            t.switches,
            t.ticks_run,
            t.ticks_run * 100 / total_ticks
        );
    }
}

fn cmd_metrics() {
    use core::sync::atomic::Ordering;
    serial_println!(
        "compositor: {} fps, frame time {} us (last) / {} us (1s max), {} frames total, mode: {}",
        crate::metrics::FPS.load(Ordering::Relaxed),
        crate::metrics::FRAME_TIME_US_LAST.load(Ordering::Relaxed),
        crate::metrics::FRAME_TIME_US_MAX.load(Ordering::Relaxed),
        crate::metrics::FRAMES_PRESENTED.load(Ordering::Relaxed),
        if crate::metrics::FORCE_FULL_REDRAW.load(Ordering::Relaxed) {
            "naive full redraw"
        } else {
            "dirty-rect"
        }
    );
    serial_println!(
        "input latency (mouse IRQ -> pixels presented): {} us last, {} us avg",
        crate::metrics::INPUT_LATENCY_US_LAST.load(Ordering::Relaxed),
        crate::metrics::INPUT_LATENCY_US_EMA.load(Ordering::Relaxed)
    );
    let c = crate::fs::simplefs::cache_stats();
    serial_println!(
        "block cache: {}, {} hits / {} misses, {}/{} blocks resident",
        if c.enabled { "on" } else { "off" },
        c.hits,
        c.misses,
        c.resident,
        c.capacity
    );
}

fn cmd_fscache(arg: &str) {
    match arg {
        "on" => {
            crate::fs::simplefs::set_cache_enabled(true);
            serial_println!("block cache enabled");
        }
        "off" => {
            crate::fs::simplefs::set_cache_enabled(false);
            serial_println!("block cache disabled (and emptied)");
        }
        _ => serial_println!("usage: fscache <on|off>"),
    }
}

fn cmd_redraw(arg: &str) {
    use core::sync::atomic::Ordering;
    match arg {
        "naive" => {
            crate::metrics::FORCE_FULL_REDRAW.store(true, Ordering::Relaxed);
            serial_println!("compositor: forcing full-screen redraw every frame");
        }
        "dirty" => {
            crate::metrics::FORCE_FULL_REDRAW.store(false, Ordering::Relaxed);
            serial_println!("compositor: dirty-rect mode restored");
        }
        _ => serial_println!("usage: redraw <naive|dirty>  (check results with 'metrics')"),
    }
}

fn cmd_fault(arg: &str) {
    match arg {
        "breakpoint" => x86_64::instructions::interrupts::int3(),
        "double" => trigger_stack_overflow(),
        "page" => unsafe {
            let ptr = 0x0dea_dbee_f000u64 as *mut u8;
            *ptr = 42;
        },
        "gpf" => trigger_gpf(),
        "" => serial_println!("usage: fault <breakpoint|page|gpf|double>"),
        other => serial_println!(
            "unknown fault type: {} (try: breakpoint, page, gpf, double)",
            other
        ),
    }
}

fn cmd_alloctest() {
    // Part 1: stress the heap allocator with Vec/Box, freeing out of order.
    serial_println!("--- heap stress: Vec/Box out-of-order alloc+free ---");
    {
        let mut boxes: Vec<Box<[u8; 4096]>> = Vec::new();
        for i in 0..20u32 {
            boxes.push(Box::new([i as u8; 4096]));
        }
        boxes.remove(5);
        boxes.remove(10);
        boxes.truncate(3);
        serial_println!(
            "allocated 20 boxes, freed out of order, {} remain",
            boxes.len()
        );
    } // everything drops here
    serial_println!("heap stress complete (no crash means the free list held up)");

    // Part 2: stress the physical frame allocator directly, out of order,
    // and verify the free-frame count returns to where it started.
    serial_println!("--- frame allocator stress: alloc/dealloc out of order ---");
    let free_before = crate::memory::frame_allocator::free_frame_count();
    serial_println!("free frames before: {}", free_before);

    let mut frames = Vec::new();
    for _ in 0..16 {
        if let Some(f) = crate::memory::frame_allocator::alloc_frame() {
            frames.push(f);
        }
    }
    // Dealloc out of order: every third frame first, then the rest.
    let mut i = 0;
    while i < frames.len() {
        let f = frames.remove(i);
        unsafe { crate::memory::frame_allocator::dealloc_frame(f) };
        i = if i + 2 < frames.len() { i + 2 } else { 0 };
        if frames.is_empty() {
            break;
        }
    }
    for f in frames {
        unsafe { crate::memory::frame_allocator::dealloc_frame(f) };
    }

    let free_after = crate::memory::frame_allocator::free_frame_count();
    serial_println!("free frames after: {}", free_after);
    if free_after == free_before {
        serial_println!("alloctest OK: all frames reclaimed, no leak");
    } else {
        serial_println!(
            "alloctest WARNING: free count mismatch ({} vs {}) -- possible leak",
            free_before,
            free_after
        );
    }
}

#[inline(never)]
#[allow(unconditional_recursion)]
fn trigger_stack_overflow() {
    trigger_stack_overflow();
    core::hint::black_box(0);
}

fn trigger_gpf() {
    use x86_64::instructions::segmentation::{Segment, CS};
    use x86_64::structures::gdt::SegmentSelector;
    use x86_64::PrivilegeLevel;
    unsafe {
        CS::set_reg(SegmentSelector::new(99, PrivilegeLevel::Ring0));
    }
}
