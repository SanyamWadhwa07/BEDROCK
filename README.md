# Bedrock

A `no_std` Rust operating system kernel, booted from bare metal with no Linux (or any other OS) underneath: its own bootloader integration, physical/virtual memory manager, interrupt handling, preemptive scheduler, framebuffer graphics stack with dirty-rect compositing, PS/2 input drivers, a window manager, and native GUI apps (a terminal, a combined file browser/text editor) backed by a real on-disk filesystem over a hand-rolled ATA PIO driver.

Built as a systems-engineering portfolio project: the goal is to be a genuine kernel + GUI desktop in the lineage of [Redox OS](https://www.redox-os.org/) or the ["Writing an OS in Rust"](https://os.phil-opp.com/) (blog_os) series taken further, not a Linux-hosted simulation and not a "custom Linux distro" (real Linux kernel + custom desktop environment).

## What this is / is not

**Is:**
- A real kernel: its own GDT/IDT/TSS, its own two-stage physical frame allocator + virtual memory manager, its own preemptive round-robin scheduler with a hand-written `naked fn` context switch, its own framebuffer graphics + dirty-rect compositor, its own IRQ-driven PS/2 keyboard/mouse drivers, its own window manager, its own on-disk filesystem (SimpleFS) over a hand-rolled ATA PIO driver.
- Single-address-space, **ring-0-only, monolithic**: every task (drivers, window manager, apps) is a kernel thread. There is no syscall boundary, no ELF loader, no per-process page tables, no ring-3 isolation. This is the single biggest, deliberately-documented scope cut versus a microkernel or versus Redox's process model -- see [DESIGN.md](DESIGN.md) for the full reasoning. Cutting ring-3 isolation put a multi-month subproject's worth of time back into the GUI stack instead.
- Single-core (no APIC/SMP), BIOS boot only (no UEFI), no networking, no USB.

**Is not:** a Linux distribution, a hypervisor guest OS, or anything with process isolation or a security boundary between "apps." Every window-manager app in this kernel could read every other task's memory if it wanted to -- there is no MMU-enforced separation between tasks, only cooperative convention.

## Architecture

```
Native Apps: Terminal | File Browser + Text Editor   (kernel threads, immediate-mode drawing)
Window Manager / Compositor: z-order, hit-test, drag/resize, focus routing -> dirty-rect merge
Filesystem: SimpleFS (superblock/inode/bitmap) over an ATA PIO driver
Input Drivers: PS/2 keyboard (IRQ1) | PS/2 mouse (IRQ12) -> lock-free SPSC ring buffer
Graphics: linear framebuffer, back-buffer, 2D primitives, bitmap font, dirty-rect compositing
Scheduler: PIT timer IRQ -> schedule() -> naked-fn context switch, round-robin ready queue
Interrupts/Exceptions: IDT, GDT+TSS+IST double-fault stack, remapped 8259 PIC
Memory Manager: two-stage frame allocator (bump -> bitmap), OffsetPageTable, heap bootstrap
Boot: `bootloader` crate (BIOS) -> BootInfo -> entry_point!(kernel_main)
```

Each layer is built on a named, classic data structure or algorithm -- see [DESIGN.md](DESIGN.md#dsa--algorithms-map) for the full map (bitmap allocator, round-robin circular queue, rectangle-union dirty tracking + painter's algorithm, lock-free SPSC ring buffer, a PS/2 packet-reassembly finite state machine, shallow-fanout inode indexing, and more).

## Quickstart

Requires a pinned nightly Rust toolchain (see `rust-toolchain.toml`, installed automatically by `rustup` on first build) and QEMU (`qemu-system-x86_64`) on `PATH`.

```
cargo xtask run              # debug build, boots in QEMU
cargo xtask run --release    # release build -- much faster to boot; recommended
```

The first run creates `fs.img`, a 4 MiB disk image for SimpleFS, reused (not recreated) on every subsequent run so filesystem writes persist across reboots exactly like a real second hard disk. Delete it by hand to force a fresh format.

Once booted, the on-screen window manager starts with three windows: **Stats** (live scheduler task counters + a bouncing box), **Terminal** (`help`, `ls`, `cat <name>`, `write <name> <text>` over the real filesystem), and **Editor** (arrow keys + Enter to open a file from the listing, `n` to create a new one, type to edit, F2 to save, Escape to go back to the listing). Drag titlebars to move windows, drag the bottom-right corner to resize, click the red button to close.

A serial debug shell is also available (`help`, `echo`, `meminfo`, `alloctest`, `fault <breakpoint|page|gpf|double>`) -- useful for exercising the memory manager and fault handling directly without the GUI.

## Known limitations (honest, not hidden)

- **No ring-3 isolation.** Every task is a trusted kernel thread; see "What this is / is not" above.
- **No SMP.** Single core, legacy 8259 PIC rather than APIC -- APIC only pays off once there's more than one core to route interrupts to.
- **No swap, no demand paging.** All mapped memory is resident.
- **BIOS boot only.** UEFI was scoped as an explicit stretch goal and not attempted this session.
- **ATA PIO, not AHCI/DMA.** Simple, correct, and slow by design -- see the benchmarks below. A real driver moving real workloads would want DMA.
- **SimpleFS is not crash-safe/journaled**, and its root directory is flat (a single fixed data block, no subdirectories) -- a deliberate scope cut to keep Phase 7 focused on proving read+write+persistence end-to-end. The inode format (direct + single-indirect block pointers) supports a general directory tree if that's ever extended.
- **No block cache.** Every filesystem access goes straight to a fresh ATA PIO round trip; an LRU block cache was scoped as a stretch goal and not implemented this session (see benchmarks -- this is exactly why throughput is as low as it is).
- **No MLFQ / priority scheduling.** The scheduler is plain round-robin; a multilevel feedback queue was scoped as a stretch goal and not attempted.
- **No FAT32 interop.** Read-only FAT32 support was scoped as an optional stretch and not attempted -- SimpleFS is the only filesystem this kernel understands.

None of the above are oversights discovered late -- they're the scope cuts named up front in [DESIGN.md](DESIGN.md), traded deliberately for finishing a complete, working GUI stack end to end.

## Benchmarks

All numbers below were captured in this project's actual development sandbox: QEMU 11.0 on Windows, **no hardware acceleration available (pure TCG software emulation)** -- meaningfully slower than KVM/WHPX-accelerated QEMU or real hardware would be, especially for anything CPU- or I/O-instruction-heavy like PIO transfers. Treat the absolute numbers as sandbox-specific, not representative of real-hardware performance; the methodology and relative comparisons (e.g. write vs. read) are still meaningful.

| Phase | Metric | Result | Method |
|---|---|---|---|
| 2 | Frame allocator reclamation | Free-frame count identical before/after `alloctest` (30370 -> 30370 in one capture) after fuzzed alloc/free-out-of-order | Shell `alloctest` command, frame counts printed before/after |
| 2 | Page-fault fault-injection | Deliberate write to an unmapped page caught cleanly, exact faulting `CR2` printed, no corruption | Shell `fault page` |
| 3 | Scheduler fairness | Five always-running tasks (no voluntary yield) interleave proportionally to their own busy-loop delay under round-robin preemption; two-task ping-pong correctness confirmed | Serial capture of per-task counters across two full rotation cycles |
| 7 | ATA PIO sequential write | 256 KB in 3.50s -> **~73 KB/s** | 64 sequential 4KiB-block writes, timed against the 100Hz PIT tick counter |
| 7 | ATA PIO sequential read | 256 KB in 1.04s -> **~246 KB/s** | 64 sequential 4KiB-block reads, same methodology |
| 7 | Filesystem persistence | A file written on one QEMU boot was read back correctly, byte-for-byte, by a second independent QEMU process against the same disk image | `fs.img` reused across boots; verified via a temporary self-test, see PLAN.md's Phase 7 log |
| Whole project | Cold boot to shell-ready | **~9.9s** (release build) | Host-side wall-clock timestamp at QEMU launch vs. the `entering serial shell` log line |

**Not yet measured** (honest gaps, not fabricated numbers): context-switch latency in cycles (would need either an RDTSC-bracketed benchmark that bypasses the scheduler's round-robin ready queue entirely, since in-band measurement is dominated by other tasks' ~50ms time slices, or careful RDTSC instrumentation inside the hand-written `switch_to` asm -- judged too risky to add without dedicated verification time this session, given it's explicitly the highest-risk unsafe surface in the kernel); redraw FPS naive-vs-dirty-rect comparison; PS/2 input latency (IRQ timestamp to visible pixel update); frame time under active window drag/resize. These are real future work, not skipped silently -- see DESIGN.md and PLAN.md's progress log for the reasoning behind each.

## Testing

- **Host-testable logic**: none of the kernel's core algorithms (bitmap allocator, round-robin queue, dirty-rect merge, PS/2 packet FSM, SimpleFS inode/bitmap layout) were split into standalone `no_std` lib crates this session -- they currently live directly in the kernel binary. Splitting them into `libs/*` crates for ordinary `cargo test` coverage (as planned) is real future work; see PLAN.md.
- **Bare-metal verification**: every phase was verified by an actual QEMU boot with captured serial output, not just "it compiles." Where this sandbox had no path to live interactive keyboard/mouse input, a temporary self-test exercised the real production code paths directly (constructing real `WindowManager`/`SimpleFS` state and asserting on it), was captured passing, and was then deleted -- see PLAN.md's progress log for each phase's captured pass/fail output. The one genuine bug the sandbox's limits couldn't fully rule out is real interactive GUI behavior (drag/resize/type by hand) -- someone with a real display and PS/2-capable QEMU frontend should exercise this directly before calling it fully proven.
- **Fault injection**: the serial shell's `fault <breakpoint|page|gpf|double>` commands deliberately trigger each exception class and verify the kernel produces a clean diagnostic instead of a silent triple-fault reboot.

## Repository layout

```
kernel/         the kernel itself (see kernel/src/{memory,task,graphics,input,wm,fs}/)
xtask/          build + QEMU launch tooling (`cargo xtask run`)
PLAN.md         the full phased build plan and a detailed, dated progress log with every
                real bug found during bring-up and how it was diagnosed and fixed
DESIGN.md       locked design decisions, the hardest components explained, the DSA map,
                and an interview-defense checklist for every major tradeoff
```
