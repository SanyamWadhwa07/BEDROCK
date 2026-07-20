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
Filesystem: SimpleFS (superblock/inode/bitmap) -> LRU block cache (write-through) -> ATA PIO driver
Input Drivers: PS/2 keyboard (IRQ1) | PS/2 mouse (IRQ12) | COM1 serial (IRQ4) -> lock-free SPSC ring buffers
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

The first run creates `fs.img`, a disk image holding SimpleFS's 4 MiB plus a small scratch region past it that `bench ata` writes to. It is reused rather than recreated on every subsequent run, so filesystem writes persist across reboots exactly like a real second hard disk. Delete it by hand to force a fresh format.

Once booted, the on-screen window manager starts with three windows:

- **Stats**: live scheduler task counters, compositor FPS and frame time, and a bouncing box.
- **Terminal**: `ls`, `cat <name>`, `write <name> <text>`, `rm <name>`, `mv <old> <new>`, and `df` against the real filesystem.
- **Editor**: arrow keys and Enter to open a file from the listing, `n` for a new one, `D` to delete the selected one, type to edit, F2 to save, Escape to go back to the listing.

Drag titlebars to move windows, drag the bottom-right corner to resize, click the red button to close.

A serial debug shell runs on COM1 alongside the GUI, driven by IRQ4 rather than polling. Besides the original `help` / `echo` / `meminfo` / `alloctest` / `fault <breakpoint|page|gpf|double>`, it now exposes the instrumentation the benchmarks below were captured with:

| Command | What it does |
|---|---|
| `bench <ata\|alloc\|fs\|ctxswitch\|all>` | Runs the in-kernel benchmark suite and prints timings. Every number in the table below is reproducible this way. |
| `metrics` | Live compositor FPS, frame time, input latency, and block-cache hit/miss counters. |
| `fscache <on\|off>` | Enables or disables the LRU block cache at runtime, so the same workload can be measured both ways in one boot. |
| `redraw <naive\|dirty>` | Forces a full-screen repaint every frame, or restores dirty-rect compositing. This is what makes the compositing decision measurable instead of merely asserted. |
| `tasks` | Per-task switch counts and CPU share, for checking scheduler fairness. |

The benchmarks are wired into the shipped kernel rather than living in a temporary harness that gets deleted after capture, which is how the earlier phases' numbers were produced.

## Known limitations (honest, not hidden)

- **No ring-3 isolation.** Every task is a trusted kernel thread; see "What this is / is not" above.
- **No SMP.** Single core, legacy 8259 PIC rather than APIC -- APIC only pays off once there's more than one core to route interrupts to.
- **No swap, no demand paging.** All mapped memory is resident.
- **BIOS boot only.** UEFI was scoped as an explicit stretch goal and not attempted this session.
- **ATA PIO, not AHCI/DMA.** Simple, correct, and slow by design -- see the benchmarks below. A real driver moving real workloads would want DMA.
- **SimpleFS is not crash-safe/journaled**, and its root directory is flat (a single fixed data block, no subdirectories) -- a deliberate scope cut to keep Phase 7 focused on proving read+write+persistence end-to-end. The inode format (direct + single-indirect block pointers) supports a general directory tree if that's ever extended.
- **Write-through block cache, no write-back.** Reads are served from an LRU block cache; writes always reach the disk before returning. That keeps durability identical to having no cache at all, and it also means writes get no speedup whatsoever from the cache. A write-back cache with an explicit flush would be the next step, and it needs a crash-consistency story SimpleFS does not currently have.
- **No MLFQ / priority scheduling.** The scheduler is plain round-robin; a multilevel feedback queue was scoped as a stretch goal and not attempted.
- **No FAT32 interop.** Read-only FAT32 support was scoped as an optional stretch and not attempted -- SimpleFS is the only filesystem this kernel understands.

None of the above are oversights discovered late -- they're the scope cuts named up front in [DESIGN.md](DESIGN.md), traded deliberately for finishing a complete, working GUI stack end to end.

## Benchmarks

All numbers below were captured in this project's actual development sandbox: QEMU 11.0 on Windows, **no hardware acceleration available (pure TCG software emulation)** -- meaningfully slower than KVM/WHPX-accelerated QEMU or real hardware would be, especially for anything CPU- or I/O-instruction-heavy like PIO transfers. Treat the absolute numbers as sandbox-specific, not representative of real-hardware performance; the methodology and relative comparisons (e.g. write vs. read) are still meaningful.

Everything marked Phase 9 comes from the `bench` and `metrics` commands built into the shipped kernel, so anyone who boots it can reproduce them. Cycle-level timings go through a TSC calibrated against the PIT at boot, cross-checked against raw tick counts.

### Block cache, on versus off

The same `bench fs` workload (create, write 64 KiB, read back, list, delete), run in one boot with the cache warm and then with `fscache off`. The point of the comparison is that it is one-sided in exactly the way write-through predicts: reads collapse, writes barely move.

| Operation | Cache off | Cache on (warm) | Change |
|---|---|---|---|
| Read 64 KiB | 305,428 us (~209 KiB/s) | **125 us (~512,000 KiB/s)** | ~2,400x |
| List directory | 2,958 us | **51 us** | ~58x |
| Create file | 13,466 us | **12,204 us** | ~1.1x |
| Write 64 KiB | 1,689,687 us (~37 KiB/s) | **1,284,509 us (~49 KiB/s)** | ~1.3x |
| Delete file | 1,032,961 us | **866,899 us** | ~1.2x |

Reads become memory-speed once blocks are resident. Writes gain only the incidental speedup of their read-modify-write of the bitmaps and inode hitting cache, because the data itself still goes to disk before the call returns. A cold first pass through this workload measured 87,912 KiB/s on read (67 cache hits, 5 misses) against 512,000 KiB/s fully warm.

### Dirty-rect compositing versus naive full redraw

Design decision #5 was the bet that dirty-rect bookkeeping pays for itself. `redraw naive` forces the strawman (mark the whole screen dirty every frame) at runtime, so both modes can be measured in the same boot, against the same three windows, with the same animation running.

| Mode | FPS | Frame time (last) | Frame time (worst in 1s) |
|---|---|---|---|
| Naive full redraw | 1 | 679,058 us | 714,616 us |
| Dirty-rect | 5 | 14,303 us | 14,457 us |

**About 47x on frame time.** The gap is this wide because the screen is 1280x720 at 3 bytes per pixel, so a full repaint moves ~2.7 MB through the compositor and then across the MMIO framebuffer every frame, while a typical frame here only actually changes a cursor and a couple of small window regions. Under TCG software emulation the MMIO copy is especially expensive, so treat 47x as the sandbox's number rather than a universal one. The direction is not sandbox-specific.

The absolute FPS is low in both modes for the same reason everything else here is slow: no hardware acceleration, and the compositor is one of seven round-robin tasks sharing a single emulated core.

### Everything else

| Phase | Metric | Result | Method |
|---|---|---|---|
| 2 | Frame allocator reclamation | Free-frame count identical before/after `alloctest` (30370 -> 30370 in one capture) after fuzzed alloc/free-out-of-order | Shell `alloctest` command, frame counts printed before/after |
| 2 | Page-fault fault-injection | Deliberate write to an unmapped page caught cleanly, exact faulting `CR2` printed, no corruption | Shell `fault page` |
| 3 | Scheduler fairness | Five always-running tasks (no voluntary yield) interleave proportionally to their own busy-loop delay under round-robin preemption; two-task ping-pong correctness confirmed | Serial capture of per-task counters across two full rotation cycles |
| 9 | Scheduler fairness, quantified | All 7 tasks landed on identical counters: 63 switches and 315 ticks each, **14% CPU apiece**, with no task drifting from the others | Shell `tasks`, which reports per-task switch counts and CPU share |
| 9 | **Context-switch latency** | **~269 cycles, ~54 ns**, per one-way switch | 100,000 ping-pong round trips (200,000 switches) against a dedicated fiber with interrupts off, bypassing the scheduler entirely. Shell `bench ctxswitch` |
| 9 | Allocator throughput | 14,400 alloc/free ops in 10.75ms -> **~1.34M ops/s** | 200 rounds of Box/Vec churn plus 16 frame alloc/free each. Shell `bench alloc` |
| 7/9 | ATA PIO sequential write | 256 KiB in 2.93s -> **~87 KiB/s** (PIT cross-check: 3.16s) | 64 sequential 4KiB-block writes to a scratch region past the filesystem. Shell `bench ata` |
| 7/9 | ATA PIO sequential read | 256 KiB in 0.68s -> **~378 KiB/s** (PIT cross-check: 0.70s) | 64 sequential 4KiB-block reads, same methodology. This is the raw floor the block cache above is measured against |
| 7 | Filesystem persistence | A file written on one QEMU boot was read back correctly, byte-for-byte, by a second independent QEMU process against the same disk image | `fs.img` reused across boots; verified via a temporary self-test, see PLAN.md's Phase 7 log |
| Whole project | Cold boot to shell-ready | **~9.9s** (release build) | Host-side wall-clock timestamp at QEMU launch vs. the `entering serial shell` log line |

**Still not measured** (honest gaps, not fabricated numbers): PS/2 input latency and frame time under active drag/resize. The instrumentation for both exists and ships (`metrics` reports mouse-IRQ-to-presented-pixel latency, stamped in the IRQ12 handler and closed out just after `present()`), but capturing them needs a human moving a real mouse. The sandbox these numbers were taken in runs QEMU headless, where nothing generates mouse packets, so `metrics` honestly reports 0. Anyone running this with a display can read the real figure straight off the Stats window.

Everything else on the earlier "not yet measured" list is now in the tables above. Context-switch latency in particular was scoped out in Phase 8 as too risky to instrument; the approach that worked avoided touching `switch_to`'s assembly at all, ping-ponging against a dedicated fiber outside the scheduler instead.

## Testing

- **Host-testable logic**: the algorithms that are pure logic rather than hardware access live in `no_std` crates under `libs/`, built for the host as well as the kernel, and covered by ordinary `cargo test` (36 tests, run in CI on every push):
  - `bedrock-mm`: the frame-allocator bitset. Includes a randomized alloc/free stress test asserting the property the kernel's stability rests on, that a frame is never handed out twice before being freed.
  - `bedrock-gfx`: rectangle algebra and the dirty-rect union. Includes a randomized test that whatever the tracker returns always covers every pixel that was marked dirty, since under-coverage would leave stale pixels on screen.
  - `bedrock-input`: the PS/2 mouse packet FSM, including a regression test for the stray 0xFA ACK byte that desynchronized the stream during Phase 5.
  - `bedrock-lru`: the filesystem block cache, checked against a naive reference model over 100,000 randomized operations.

  What deliberately stayed in the kernel binary is the part that cannot run on a host: interrupt handlers, inline assembly, port I/O, and MMIO. The 8042 controller dance stays in `kernel/src/input/mouse.rs`; only the byte-stream decoding moved out. SimpleFS's inode and directory logic is still coupled to the ATA driver and has not been split, which is the obvious next extraction.
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
