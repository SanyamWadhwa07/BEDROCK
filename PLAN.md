# Bedrock — a real from-scratch OS kernel + GUI in Rust

## Context

The user wants a flagship SDE-resume project for infra/systems-focused companies (AWS, Cloudflare, Databricks, MongoDB, database/infra startups) that is genuinely unique — not another "clone" repo. An earlier draft of this plan scoped "Bedrock" as a userspace-only Rust runtime (allocator + cooperative scheduler + VFS running as a normal Linux process); the user explicitly rejected that as incomplete, wanting a real custom OS with its own UI and memory manager — comparable in ambition to Redox OS (a real Rust OS with its own kernel and its own GUI desktop) rather than a Linux-hosted simulation, and specifically not a Garuda-Linux-style "custom Linux distro" (which is just the real Linux kernel with a custom desktop environment — much less of a systems-engineering signal).

This plan **supersedes** the earlier Pyrite/Strata/userspace-Bedrock 4-week plan. Two previously-planned companion projects — **Pyrite** (a Redis clone) and **Strata** (a Kafka clone) — are **formally paused**. All effort goes to Bedrock until it reaches its Month-1 milestone bundle (boots, has real virtual memory, has a real preemptive scheduler). Pyrite/Strata are revisited only after that point, and are out of scope for this plan.

Bedrock is a `no_std` Rust kernel with no Linux underneath: its own bootloader integration, its own physical/virtual memory manager, its own interrupt handling, its own preemptive scheduler, its own framebuffer graphics stack, its own input drivers, its own window manager/compositor, and 2-3 native GUI apps proving the whole stack works end to end. Realistic timeline: **3-6+ months part-time** (the user accepted this extension after initially wanting a 1-month project at a much smaller scope). Month 1's target is a complete, demoable milestone on its own — boot to shell, real paging, real preemptive multitasking — reached *before* any GUI work starts, so progress is visible early rather than a months-long black box.

Repo location: **`D:\Systems\bedrock`** — a single cargo workspace (not three separate repos, since Pyrite/Strata are paused and this is one project now).

---

## Locked design decisions (not open menus)

| # | Decision | Why |
|---|---|---|
| 1 | **x86_64**, boot via the `bootloader` crate (pin `=0.11.15`) + `x86_64` crate (pin `=0.15.4`), BIOS boot path first, UEFI as later stretch | Best-documented, best-tooled solo path (the "Writing an OS in Rust" / blog_os lineage); hand-rolled multiboot2/limine or ARM/RISC-V trade kernel-engineering time for boot-protocol trivia with far worse debugging support |
| 2 | Physical memory: **two-stage frame allocator** — bootstrap linear allocator over the BootInfo memory map, promoted to a **bitmap allocator** (1 bit/4KiB frame) once the heap exists. Virtual memory: bootloader's **offset-mapped physical memory** + higher-half kernel | Bitmap is simple/robust for fixed 4KiB units (buddy's power-of-2 rounding buys nothing here); offset-mapped physical memory avoids self-referential recursive page-table bugs |
| 3 | IDT + **TSS/IST-backed double-fault stack**, legacy **8259 PIC** (not APIC), single-core only | PIC is sufficient for one core; APIC only pays off for SMP, explicitly out of scope |
| 4 | **Monolithic, single-address-space, ring-0-only kernel.** All tasks (drivers, WM, apps) are kernel threads. Preemptive round-robin scheduler, context switch via a naked-fn callee-saved-register swap | Ring-3 isolation (syscalls, ELF loader, per-process CR3) is itself a multi-month project; cutting it puts 100% of remaining time into the GUI stack, the real differentiator. This is the single biggest, explicitly-documented scope cut vs. Redox |
| 5 | Bootloader-provided linear framebuffer, software back-buffer, **dirty-rectangle compositing** (not naive full redraw), fixed ~30-60Hz redraw tick | Bootloader crate already does GOP/VBE mode-setting; dirty-rect is a real, benchmarkable perf decision and a strong interview signal |
| 6 | PS/2 keyboard (`pc-keyboard` crate) + PS/2 mouse (hand-rolled 3-byte packet parser), both **IRQ-driven** | Correct OS approach, reuses Phase-1 IDT/PIC infra; USB HID is dramatically harder for no benefit in QEMU |
| 7 | Window/surface struct with z-order `Vec`, **immediate-mode** app drawing (no retained-mode widget toolkit) | A full widget-tree/layout engine is its own project; immediate-mode (egui-style) still proves the GUI stack end-to-end |
| 8 | Phases 1-6: trivial in-memory **MemFS**. Phase 7: real on-disk **SimpleFS** (superblock + inode table + bitmap) over an **ATA PIO** disk driver. Read-only FAT32 kept as an optional later stretch | MemFS keeps the GUI milestone unblocked by disk I/O; a custom FS is more original resume signal than implementing the FAT32 spec; ATA PIO avoids PCI enumeration/DMA complexity AHCI would require |

---

## High-Level Design

```
Native Apps: Terminal | File Browser | Text Editor  (kernel threads, immediate-mode drawing)
Window Manager / Compositor: z-order, hit-test, drag/resize, focus routing → dirty-rect merge
Filesystem: SimpleFS (superblock/inode/bitmap) over ATA PIO driver  [Phases 1-6: MemFS]
Input Drivers: PS/2 keyboard (IRQ1) | PS/2 mouse (IRQ12) → ring buffer
Graphics: linear framebuffer, back-buffer, 2D primitives, bitmap font
Scheduler: PIT timer IRQ → schedule() → naked-fn context switch, round-robin ready queue
Interrupts/Exceptions: IDT, GDT+TSS+IST double-fault stack, PIC
Memory Manager: frame allocator (bootstrap→bitmap), OffsetPageTable, heap bootstrap
Boot: `bootloader` crate (BIOS) → BootInfo → entry_point!(kernel_main)
```

---

## LLD — the 5 hardest components

**1. Physical + virtual memory manager.** Stage A: bump-allocate frames straight from `BootInfo.memory_regions` (no `dealloc`), used only to build initial page tables and back a small fixed heap (256KB-1MB). Stage B: once that heap is live, allocate a bitmap (1 bit/frame across all usable RAM) *from the heap*, mark Stage-A-consumed frames used, and make it the real global frame allocator (scans from a last-freed cursor hint, not from zero, to avoid O(n) restarts). Paging via bootloader's fixed offset-mapped physical memory + `OffsetPageTable`; new mappings (heap growth, per-task stacks, window buffers) go through `map_to()` using the bitmap allocator as the `FrameAllocator`. Every per-task kernel stack gets an unmapped guard page directly below it so overflow page-faults immediately instead of corrupting adjacent memory.

**2. Interrupts/exceptions.** GDT + a TSS whose `interrupt_stack_table[0]` points at a small dedicated static stack used *only* by the double-fault handler. IDT covers breakpoint, double-fault (`.set_stack_index(0)`), page-fault (reads `CR2`), GPF, and later the remapped hardware IRQs (PIC remapped via the `pic8259` crate so IRQs land at vectors 32-47). Why the IST stack matters: a fault inside a fault handler, combined with an already-corrupted current stack (e.g. a stack overflow), triggers a double fault; without a guaranteed-good stack for that handler, the CPU can't deliver the exception at all — **triple fault → silent instant reboot**, the single most confusing bare-metal failure mode. The IST switch happens at the hardware level before any handler code runs, decoupling it from whatever stack was faulting — this turns "mystery reboot" into a clean diagnostic printout.

**3. Preemptive scheduler.** TCB: `{ id, saved_rsp, kernel_stack, state }`. Since the kernel is single-address-space/monolithic, no CR3 reload is needed on switch. Run queue: round-robin `VecDeque<TaskId>`, fixed time slice in PIT ticks. Context switch: a `naked fn switch_to(prev, next)` pushes System-V callee-saved registers (`rbx, rbp, r12-r15`) onto the current stack, swaps `rsp` between TCBs, pops registers, `ret`s. The timer IRQ stays thin (bump tick count, call `schedule()` if the slice expired); because every non-running task's stack is arranged to look "mid-call inside `switch_to`," the final `ret` unwinds into the *next* task's own previously-interrupted handler body, which does its own `iretq` from its own saved frame — correctly resuming where it left off. (This is the standard resolution used by xv6/Redox-lineage kernels for the fact that preemptive switching doesn't compose cleanly with Rust's `x86-interrupt` ABI — it's precisely why blog_os itself stops at cooperative async instead of going this far.) The first thing built on top of this must be a two-task ping-pong correctness test, run before anything else touches the scheduler.

**4. Framebuffer + compositor.** Heap-allocated `Vec<u32>` back buffer matching the bootloader-reported `PixelFormat` (handle `Rgb`/`Bgr` explicitly — don't hardcode byte order, or colors swap on some hardware/QEMU configs). All drawing targets the back buffer. Dirty-rect tracking: a small `Vec<Rect>` per frame from window invalidation (move/resize/create/destroy/focus) and app-requested partial repaints, merged via bounding-box union, falling back to one full-screen rect if fragmentation exceeds a threshold. Per redraw tick, for each surviving rect: repaint background, composite windows back-to-front in z-order clipped to that rect, then copy just that rect to the real MMIO framebuffer. Fixed ~30-60Hz cadence off the existing timer.

**5. On-disk filesystem (SimpleFS) + ATA PIO driver.** ATA PIO over legacy fixed ports (`0x1F0-0x1F7`, primary channel) — no PCI enumeration, simple `in`/`out` per sector, adequate throughput for a demo FS. On-disk layout: `[superblock][inode bitmap][data bitmap][inode table][data blocks]`, 4KiB blocks, xv6/early-ext2-style. Inode: `{ size, block_count, direct_blocks[N], single_indirect_block, type }`. Directories are data blocks of `(name, inode)` entries. Phases 1-6 use a heap-backed MemFS (`enum Node::File(Vec<u8>) | Dir(BTreeMap<String,NodeId>)`) specifically so the GUI milestone isn't gated on disk I/O; SimpleFS only enters in Phase 7 once the text editor/file browser need real persistence for a meaningful demo.

---

## Phased roadmap

**Month-1 milestone bundle = Phases 0-3** (~5-7 weeks realistically), must be fully demoable *before any GUI work begins*. Overall project: ~18-25 weeks part-time (~4.5-6 months).

- **Phase 0 (~3-5 days) — Toolchain & scaffold.** Pin nightly via `rust-toolchain.toml`, pin `bootloader =0.11.15` / `x86_64 =0.15.4`, `x86_64-unknown-none` target, an `xtask` crate that builds a bootable BIOS image and launches QEMU (`-serial stdio -no-reboot -no-shutdown` so triple faults show as a QEMU exit, not an invisible loop), serial logging, halting panic handler. **Demo:** `cargo xtask run` boots in QEMU, serial shows a boot banner + memory-map dump, kernel idles cleanly.
- **Phase 1 (~1.5-2 weeks) — Boot to shell.** GDT+TSS+IST, IDT (breakpoint/double-fault/page-fault/GPF), PIC remap, a minimal direct-to-framebuffer bitmap-font print routine, a serial debug shell (`help`, `echo`, `meminfo`, `fault <type>`). **Demo:** on-screen banner text; shell commands over serial; `fault double` triggers a clean double-fault diagnostic instead of a silent reboot.
- **Phase 2 (~2-3 weeks) — Virtual memory + heap.** Two-stage frame allocator, `OffsetPageTable` wiring, heap bootstrap with `linked_list_allocator`. **Demo:** shell `alloctest` stresses Vec/Box alloc+free out of order and prints free-frame counts proving reclamation; touching a deliberately unmapped page triggers a clean page fault printing the faulting `CR2`.
- **Phase 3 (~2-3 weeks) — Preemptive scheduler.** PIT timer IRQ, TCB, naked-fn context switch, round-robin queue, spawn/exit/yield, guarded per-task stacks. **Demo:** 5-10 kernel tasks visibly interleaving on screen/serial; a deliberately busy-looping task does not starve the others — direct proof of true preemption; ping-pong correctness test passes. **← End of Month-1 bundle.**
- **Phase 4 (~2-3 weeks) — Full framebuffer graphics.** Back-buffer, 2D primitives, full bitmap font, dirty-rect tracking/merge, fixed redraw cadence. **Demo:** screen recording of an animated element (bouncing rect / live FPS counter) redrawing flicker-free at a measured frame rate.
- **Phase 5 (~2 weeks) — Input drivers.** PS/2 keyboard + hand-rolled mouse packet parser, IRQ-driven, ring buffer + input task. **Demo:** recording of a cursor tracking QEMU's mouse and typed characters echoing live on the framebuffer.
- **Phase 6 (~3-4 weeks) — Window manager/compositor.** Window/surface struct, z-order, hit-testing, drag/resize, focus routing, titlebars + close button, 2+ demo windows. **Demo:** recording of opening/dragging/overlapping/resizing/closing windows with correct focus and z-order.
- **Phase 7 (~3-4 weeks) — Native apps + real filesystem.** ATA PIO driver, SimpleFS, terminal (now framebuffer+keyboard driven), file browser, text editor. **Demo (centerpiece recording):** browse files → open in editor → edit → save → reboot QEMU → reopen → edit persisted on real disk.
- **Phase 8 (~2-3 weeks) — Polish, benchmarks, docs, stretch.** Full benchmark suite recorded, README/DESIGN.md finalized, known-limitations section, optional stretch (APIC, UEFI boot, read-only FAT32 interop, MLFQ scheduler) explicitly labeled as attempted-or-not-and-why.

---

## Benchmarks per milestone

- **Phase 2:** frame-allocator correctness under fuzzed alloc/free order; internal fragmentation (requested vs. reserved); page-fault fault-injection test (deliberate unmap + touch, assert clean `CR2` recovery — a real pass/fail test).
- **Phase 3:** context-switch latency (RDTSC-bracketed ping-pong, cycles/switch); switches/sec vs. task count; fairness (tick-count variance under equal-priority round robin).
- **Phase 4:** redraw FPS, naive full-redraw vs. dirty-rect, under N overlapping windows — the direct evidence for decision #5.
- **Phase 5:** approximate input latency (IRQ timestamp → visible pixel update).
- **Phase 6:** frame time under active drag/resize with multiple windows.
- **Phase 7:** sequential/random read+write MB/s via ATA PIO (expect slow — have the DMA/AHCI explanation ready); crash/persistence correctness (reboot mid-demo, verify data survives).
- **Whole project:** cold-boot-to-shell-ready time.

## Interview-defense checklist

1. `bootloader` crate vs. hand-rolled multiboot2/limine — maturity/velocity vs. knowing what a hand-rolled real→protected→long-mode transition would cost.
2. Bitmap physical allocator vs. buddy — bitmap + cursor hint is simpler and buddy's benefit (variable-size allocation) doesn't apply to fixed 4KiB frames.
3. PIC vs. APIC / no SMP — PIC is right-sized for single-core; APIC only matters for multicore, explicitly out of scope.
4. Monolithic ring-0-only kernel vs. Redox-style microkernel/ring-3 isolation — the single biggest named scope cut; know exactly what syscalls/per-process CR3/ELF loading would cost, traded for a complete GUI stack instead.
5. Dirty-rect compositing vs. naive full redraw — real bookkeeping complexity for a benchmarkable perf win.
6. Custom on-disk filesystem vs. FAT32 — original design/recovery-story signal vs. spec-implementation.
7. ATA PIO vs. AHCI — deliberate scope control, honestly acknowledged against the throughput benchmark.

## README.md / DESIGN.md expectations

Architecture diagram (the HLD layering above); a **"what this is / is not"** section honestly comparing to Linux and to Redox (monolithic ring-0-only vs. microkernel + ring-3 isolation); screenshots/recordings per GUI phase (Phase 7's edit-and-persist recording is the centerpiece); benchmark tables with methodology (host specs, QEMU version/flags, RDTSC calibration); known limitations stated proudly as future work (no SMP, no ring-3 isolation, no networking, no USB, PIO not DMA, non-journaled FS, no swap); quickstart (`cargo xtask run`); a testing section covering host-testable lib crates, QEMU bare-metal integration tests, fault-injection tests, and the FS crash/persistence test.

## Known Rust no_std/bare-metal traps, de-risked early

1. **Can't unit-test kernel code on a host.** Split pure logic into host-testable `no_std` lib crates under `libs/`: `bedrock-mm` (bitmap logic against a fake memory-map fixture), `bedrock-sched` (TCB/run-queue logic minus the real asm switch), `bedrock-fs` (inode/bitmap/directory logic against an in-memory fake disk — fully `cargo test`-able even though the real ATA driver isn't), `bedrock-gfx` (dirty-rect merge algorithm). Only thin glue (interrupt handlers, inline asm, MMIO) lives in the kernel binary itself.
2. **Things that must run bare-metal** (context switch, real interrupts, real paging) — use `custom_test_frameworks` with a QEMU `isa-debug-exit` device convention so these still report pass/fail without a human watching the screen.
3. **Debugging** — serial logging (`uart_16550`/COM1) as "console.log from ring 0"; QEMU+GDB stub (`-s -S`, kernel ELF debug symbols) for context-switch/page-fault bugs; keep a near-unoptimized build profile available for the hardest sessions.
4. **Double-fault → triple-fault reboot loop** — a fault inside a fault handler on an already-corrupted stack (classic once per-task stacks exist) is unrecoverable without a guaranteed-good handler stack. The TSS/IST switch happens at the hardware level before the handler runs, independent of the cause — combined with per-task guard pages, this turns stack overflows into a clean diagnosable fault instead of silent corruption or an inexplicable reboot.
5. **Unsafe/inline-asm correctness in context switching** — the callee-saved register contract must exactly match System V AMD64 or errors surface as bizarre, far-delayed crashes. De-risk with the two-task ping-pong test the moment context switching exists, and re-run it after any future touch of that function.
6. **Bootloader crate version churn** — the 0.9.x series (what most tutorial-era posts show) differs materially from current 0.11.x (`BootloaderConfig`, `entry_point!`). Pin the exact version, commit `Cargo.lock`, read only 0.11.x docs.
7. **`x86-interrupt` ABI is unstable and shifts across nightlies** — pin a specific dated nightly via `rust-toolchain.toml`, not floating `nightly`.
8. **Framebuffer pixel format isn't fixed** — bootloader's `PixelFormat` can be `Rgb`, `Bgr`, `U8`, or `Unknown`. Handle this behind one `set_pixel(x,y,Color)` abstraction from Phase 1, or colors swap on some configs.

---

## Setup / first actions on approval

1. Create `D:\Systems\bedrock` as a single cargo workspace.
2. Scaffold in build order (also serves as the file-by-file implementation sequence):
```
D:\Systems\bedrock\
  rust-toolchain.toml
  Cargo.toml                              # workspace
  .cargo\config.toml                      # target = x86_64-unknown-none
  xtask\src\main.rs                       # Phase 0
  kernel\src\main.rs                      # Phase 0
  kernel\src\serial.rs                    # Phase 0
  kernel\src\framebuffer_text.rs          # Phase 1
  kernel\src\gdt.rs                       # Phase 1
  kernel\src\interrupts.rs                # Phase 1
  kernel\src\shell.rs                     # Phase 1
  kernel\src\memory\frame_allocator.rs    # Phase 2
  kernel\src\memory\paging.rs             # Phase 2
  kernel\src\memory\heap.rs               # Phase 2
  kernel\src\task\tcb.rs                  # Phase 3
  kernel\src\task\context_switch.rs       # Phase 3 (naked asm)
  kernel\src\task\scheduler.rs            # Phase 3
  kernel\src\task\timer.rs                # Phase 3
  kernel\src\graphics\framebuffer.rs      # Phase 4
  kernel\src\graphics\font.rs             # Phase 4
  kernel\src\graphics\dirty_rect.rs       # Phase 4
  kernel\src\graphics\compositor.rs       # Phase 4/6
  kernel\src\input\keyboard.rs            # Phase 5
  kernel\src\input\mouse.rs               # Phase 5
  kernel\src\input\event_queue.rs         # Phase 5
  kernel\src\wm\window.rs                 # Phase 6
  kernel\src\wm\zorder.rs                 # Phase 6
  kernel\src\wm\wm.rs                     # Phase 6
  kernel\src\fs\memfs.rs                  # Phase 1-6 placeholder
  kernel\src\fs\ata.rs                    # Phase 7
  kernel\src\fs\simplefs.rs               # Phase 7
  kernel\src\apps\terminal.rs             # Phase 7
  kernel\src\apps\file_browser.rs         # Phase 7
  kernel\src\apps\text_editor.rs          # Phase 7
  libs\bedrock-mm\      libs\bedrock-sched\   libs\bedrock-fs\   libs\bedrock-gfx\
  tests\                                  # QEMU custom_test_frameworks integration tests
  README.md   DESIGN.md   docs\screenshots\   docs\benchmarks\
```
3. README.md and DESIGN.md get skeleton sections from day one (pitch, "what this is/is not", architecture diagram placeholder), filled in as each phase's decisions are actually made — not written retroactively.

## Verification

- Each phase's "Demo" line above is the acceptance criterion for that phase — a specific observable behavior in QEMU (screen output, serial output, or a screen recording), not just "it compiles."
- The Phase-3 ping-pong context-switch test and the Phase-2 page-fault fault-injection test are correctness gates for the two highest-risk unsafe surfaces in the kernel — both must pass before building anything further on top of them.
- Bare-metal-only logic gets QEMU `custom_test_frameworks` tests (via `isa-debug-exit`); everything else (allocator bit-twiddling, scheduler queue logic, FS layout logic, dirty-rect merge) lives in host-testable `libs/*` crates run via ordinary `cargo test`.
- Benchmarks are recorded per-phase (not just eyeballed) into `docs/benchmarks/` so numbers in the README are reproducible.

---

## Progress Log

- **2026-07-14** — **Phase 0 complete.** Rust + QEMU installed via winget. Workspace scaffolded at `D:\Systems\bedrock` (kernel crate excluded from the xtask workspace so each can have its own `.cargo/config.toml` target).
  - Hit a real toolchain/crate compatibility bug: `bootloader` 0.11.15's bundled BIOS stage-3/stage-4 target-spec JSON uses `"rustc-abi": "x86-softfloat"`, which the newest nightly (2026-07-12) rejects — an upstream rustc rename (`x86-softfloat` → `softfloat`) landed after 0.11.15 was cut (Feb 2026). Fixed by pinning an older compatible nightly, **`nightly-2026-02-15`**, in `rust-toolchain.toml` instead of floating to newest — this is exactly the "bootloader crate/toolchain version churn" trap called out in the plan, just from the toolchain-too-new direction rather than crate-too-old.
  - `cargo xtask run` now works end-to-end: builds the kernel for `x86_64-unknown-none`, builds a bootable BIOS disk image via the `bootloader` crate, boots it in QEMU. Verified via captured serial output: SeaBIOS → bootloader stages 1-4 → ELF loaded → jump to kernel entry → **"Bedrock kernel booting..." → full memory-region dump → "Bedrock: idle."** Matches the Phase 0 demo criterion exactly.
  - Next up: Phase 1 (GDT+TSS+IST, IDT, PIC remap, on-screen framebuffer text, serial debug shell).

- **2026-07-14** — **Phase 1 complete.** GDT with a TSS/IST-backed double-fault stack, IDT (breakpoint/double-fault/page-fault/GPF), 8259 PIC remap (interrupts left masked — no real IRQ handlers until Phase 3/5), a minimal direct-to-framebuffer text writer (`noto-sans-mono-bitmap` font), and a serial debug shell (`help`, `echo`, `meminfo`, `fault <breakpoint|page|gpf|double>`).
  - Dependency-pin bug #2: `x86_64` crate resolved to the newest 0.15.5, whose unstable `Step` trait impl (`forward_overflowing`/`backward_overflowing`) didn't match our pinned nightly's std. Fixed by pinning the exact version the plan called for, `x86_64 = "=0.15.4"`.
  - Real bug found and fixed via actual boot testing: after `GDT.load()`, the CPU still held stale SS/DS/ES/FS/GS selector values left over from the bootloader's own (now-discarded) GDT. Those stale indices aliased our new, much smaller GDT (in this case landing on the TSS descriptor), so returning from the breakpoint handler's `iretq` GP-faulted instead of resuming cleanly. Fixed by explicitly nulling SS/DS/ES/FS/GS right after loading the new GDT (`kernel/src/gdt.rs`) — a real-world instance of the exact class of bug the plan's "unsafe/inline-asm correctness" trap warned about, just in segment setup rather than context-switch code.
  - Verified via full boot capture (serial output through a temporary self-test harness, since this sandbox blocks the TCP/socket access needed for true interactive serial input): `help`/`meminfo`/`echo` all work; `fault breakpoint` now returns cleanly (stack segment selector = 0/null, confirming the fix); `fault double` produces a genuine **double fault**, caught cleanly by the IST-backed handler, printing a full diagnostic and halting — not a silent triple-fault reboot. Matches the Phase 1 demo criterion exactly.
  - Next up: Phase 2 (two-stage frame allocator, `OffsetPageTable`, heap bootstrap).
