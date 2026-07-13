use crate::serial::SERIAL1;
use crate::{serial_print, serial_println};
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
        let byte = SERIAL1.lock().receive();
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
            serial_println!("commands: help, echo <text>, meminfo, fault <type>");
            serial_println!("fault types: breakpoint, page, gpf, double");
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
        "fault" => cmd_fault(arg),
        other => serial_println!("unknown command: {} (try 'help')", other),
    }
}

fn cmd_fault(arg: &str) {
    match arg {
        "breakpoint" => x86_64::instructions::interrupts::int3(),
        "double" => trigger_stack_overflow(),
        "page" => unsafe {
            let ptr = 0xdead_beef_000 as *mut u8;
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
