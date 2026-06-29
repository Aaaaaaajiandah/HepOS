# HepOS — Complete Design Reference

> **Purpose:** This document is the authoritative reference for HepOS development.
> It survives context compaction and contains everything needed to continue building.

---

## Overview

HepOS is a custom x86\_64 operating system written in Rust. It uses an **exokernel architecture** — the kernel does as little as possible (secure hardware multiplexing), pushing all OS abstractions into userspace libOS libraries. Single user, no permissions, no networking yet.

**Language:** Rust (nightly, `no_std` + `alloc`)
**Target:** x86\_64, bare metal
**Bootloader:** Limine v9.x (BIOS mode)
**Development machine:** Windows 11 with QEMU 11.x

---

## Repository Structure

```
HepOS/
├── build.ps1              # One-shot build + QEMU launch script
├── PLAN.md                # This file
├── .gitignore             # Excludes: limine/, iso_root/, hepos.iso, hepos_disk.img, kernel/target/
├── bootloader/
│   └── limine.conf        # Boot menu config
├── kernel/
│   ├── Cargo.toml         # no_std, limine 0.6, spin 0.9, alloc
│   ├── linker.ld          # Higher-half kernel linker script (0xFFFFFFFF80000000)
│   ├── .cargo/config.toml # Target: x86_64-unknown-none, build-std, linker flags
│   └── src/
│       ├── main.rs        # kmain entry, global state, task setup
│       ├── framebuffer.rs # GOP framebuffer pixel/rect/text renderer (8×8 bitmap font)
│       ├── gdt.rs         # Global Descriptor Table (null, code64, data64)
│       ├── idt.rs         # IDT + exception stubs + timer stub
│       ├── pmm.rs         # Physical Memory Manager (bitmap, pages above 1MB only)
│       ├── vmm.rs         # HHDM offset storage, phys_to_virt()
│       ├── paging.rs      # Page table walker, map_page(), map_mmio()
│       ├── heap.rs        # Bump allocator (1MB, no-free) — GlobalAlloc impl
│       ├── apic.rs        # x2APIC (MSR-based), timer at ~10ms, disables legacy PIC
│       ├── scheduler.rs   # Round-robin preemptive scheduler, context_switch (naked asm)
│       ├── pci.rs         # PCI enumeration via config space I/O ports (0xCF8/0xCFC)
│       ├── ps2.rs         # PS/2 keyboard (scancode set 1 → ASCII, circular buffer)
│       ├── mouse.rs       # PS/2 mouse AUX port (3-byte packets, relative movement)
│       ├── nvme.rs        # NVMe driver (admin + I/O queues, identify, read/write blocks)
│       ├── hepfs.rs       # Custom filesystem (flat inodes, 4KB blocks, NVMe backend)
│       ├── desktop.rs     # Desktop environment (compositor, WM, taskbar, mouse)
│       ├── serial.rs      # COM1 serial debug output (print, print_hex)
│       └── panic.rs       # Panic handler (spins forever)
└── limine/                # Cloned at build time (gitignored)
```

---

## Build System

**`build.ps1`** does everything:
1. `cargo +nightly build --release` in `kernel/`
2. Clones Limine v9.x-binary if not present
3. Assembles ISO image using `xorriso` (from MSYS2 `usr/bin/`)
4. Installs Limine BIOS stage with `limine.exe`
5. Creates `hepos_disk.img` (512MB raw NVMe disk) if not present
6. Launches QEMU

**QEMU command:**
```
qemu-system-x86_64
  -M q35
  -cpu qemu64,+x2apic     ← x2APIC required for APIC timer via MSRs
  -m 256M
  -cdrom hepos.iso
  -boot d
  -drive file=hepos_disk.img,if=none,id=nvme0,format=raw
  -device nvme,serial=heposv1,drive=nvme0
  -vga std
  -display sdl,window-close=off
  -serial stdio             ← serial debug goes to PowerShell terminal
  -no-reboot
  -no-shutdown
```

**Requirements:**
- Rust nightly with targets: `x86_64-unknown-none`, `x86_64-pc-windows-msvc`
- Components: `rust-src`, `llvm-tools`
- MSYS2 with `xorriso` in `/usr/bin/`
- QEMU 11.x at `C:\Program Files\qemu\`
- PATH must include `%USERPROFILE%\.cargo\bin`

---

## Architecture Decisions

### Exokernel Design
The kernel only does:
- Hardware multiplexing (memory pages, CPU time)
- Secure isolation
- Interrupt routing

Everything else (filesystem, scheduler policy, device abstraction) lives in "libOS" — currently in the kernel address space for simplicity. True userspace separation is a future milestone (requires syscall interface + per-process paging).

### Memory Layout
```
0x0000000000000000 – 0x000FFFFFFFFFFFFF : User virtual (future)
0xFFFF800000000000 – 0xFFFF8FFFFFFFFFFF : HHDM (Limine maps all physical RAM here)
0xFFFFFFFF80000000 – 0xFFFFFFFFFFFFFFFF : Kernel (linked here, Limine maps)
```

Limine hands us the HHDM offset via `HhdmRequest`. All physical addresses are accessible as `hhdm_offset + phys`. We use this for heap, page tables, and NVMe DMA buffers.

### Physical Memory Manager (PMM)
- **Bitmap allocator:** 1 bit per 4KB page, stored in static atomic u64 array
- **Only frees pages above 1MB** — avoids the 0xA0000–0xFFFFF reserved hole (VGA/BIOS)
- **alloc_page()** returns physical addresses; caller uses `phys_to_virt()` to access
- **MAX_PAGES = 512K** (tracks up to 2GB)
- Pages tracked: 131,072 (256MB QEMU default)

### Heap Allocator
- **Bump allocator** — allocates sequentially, dealloc is a no-op
- **256 pages = 1MB** of heap
- Assumes pages allocated from PMM are physically contiguous (true for pages above 1MB on QEMU)
- **Why bump, not free-list:** free-list had a dealloc bug (wrote header at user pointer instead of block start) causing heap corruption. Bump is provably correct. Replace with slab allocator when memory pressure matters.
- Registered as `#[global_allocator]`, gives access to `Box`, `Vec`, `String`, etc.

### Paging
- Limine sets up initial page tables (kernel + HHDM for all RAM)
- `map_page(virt, phys, flags)` walks existing PML4 → PDPT → PD → PT, allocating new table pages from PMM as needed
- `map_mmio(phys, size)` maps physical MMIO at `hhdm_offset + phys` with `NOCACHE` flag
- **Used for:** NVMe BAR (at 0xFEBD4000 on QEMU), future PCI device MMIO

### APIC & Timer
- **x2APIC only** — uses MSRs (0x800+), no MMIO mapping needed
- Legacy 8259 PIC disabled (masked out)
- Timer: periodic mode, divide-by-16, ~625,000 count ≈ 10ms per tick
- Timer fires at vector 0x20; timer_stub saves all scratch registers, calls `tick()`, sends EOI
- **Why x2APIC:** xAPIC MMIO at 0xFEE00000 is not in Limine's HHDM (MMIO, not RAM)
- QEMU must be launched with `-cpu qemu64,+x2apic`

### Scheduler
- **Round-robin preemptive**, single core
- Two tasks: `task_idle` (hlt loop), `task_blink` (desktop render + input loop)
- `context_switch(old_rsp: *mut u64, new_rsp: u64)` — naked function, saves/restores RBX/RBP/R12–R15
- Task stacks: 64KB each, allocated from heap (bump allocator)
- **Timer fires → tick() → context_switch switches stacks → iretq returns to new task**
- **Known issue:** interrupts must be disabled (CLI) during NVMe init to prevent timer firing mid-MMIO setup
- Lock ordering: SCHEDULER lock dropped BEFORE context_switch to avoid deadlock

### Virtual Memory (per-process)
- **Not yet implemented.** Currently all code runs in kernel address space.
- Future: `SYSCALL`/`SYSRET` interface, per-process PML4, ring 3 code

---

## Kernel Modules — Detailed

### framebuffer.rs
- Wraps Limine GOP framebuffer
- Stores `addr` (MMIO pointer), `width`, `height`, `pitch`, `bpp`, `r/g/b_shift`
- Pixel writes use channel shift values from Limine (correct regardless of BGR/RGB order)
- Bitmap font: 8×8 pixels, 128 entries, bit scanning LSB-first (col 0 = LSB)
- `draw_text(x, y, text, color, scale)` — scales glyphs by integer factor
- `fill_rect(x, y, w, h, color)` — bounds-checked
- `put_pixel_pub()` — public wrapper for desktop use

### gdt.rs
- 3 entries: null, kernel code (CS=0x08), kernel data (DS=0x10)
- `lgdt` via inline asm; CS reload skipped (Limine's CS is valid for long mode)
- Future: add TSS entry for ring 0 stack on syscall

### idt.rs
- 256 entries; 32 CPU exception handlers (ex0–ex31) via `exception_stub!` macro
- Exceptions with error code: 8, 10–14, 17, 21, 29, 30
- `common_stub`: saves all registers, calls `exception_handler()`, restores, iretq
- `exception_handler()`: draws red screen with exception name, vector, error code, RIP
- `timer_stub`: naked function, saves scratch regs, calls `tick()` + `eoi()`, iretq
- `set_handler(vector, fn_ptr)`: registers interrupt handlers post-init

### serial.rs
- COM1 (0x3F8), 38400 baud
- `print(s: &str)`, `print_hex(label, val: u64)`

### pci.rs
- Config space access via I/O ports 0xCF8 (address) / 0xCFC (data)
- `enumerate()` → `Vec<PciDevice>` with bus/dev/func/vendor/device/class/subclass
- Class codes: STORAGE(0x01), NETWORK(0x02), DISPLAY(0x03), BRIDGE(0x06), SERIAL(0x0C)
- Subclasses: NVME(0x08), USB(0x03), ETHERNET(0x00)
- **QEMU PCI devices found:** Host Bridge, VGA, Ethernet, NVMe, ISA Bridge, SATA/AHCI, Unknown

### ps2.rs
- Initializes PS/2 controller: disable both ports, config, enable port 1, reset, scancode set 1, enable scanning
- Scancode set 1 → ASCII lookup table (US QWERTY)
- `poll()` — checks bit 5 of status: if 0 (keyboard byte), process; if 1 (mouse byte), skip
- `read_char()` → `Option<char>`, `read_char_blocking()` → `char`
- Circular buffer (64 bytes)

### mouse.rs
- PS/2 AUX port initialization with timeout-safe reads
- `handle_byte()`: accumulates 3-byte packets; byte 0 must have bit 3 set
- Packet decode: dx = packet[1] - (flags&0x10 ? 256 : 0), dy = -(packet[2] - (flags&0x20 ? 256 : 0))
- `MOUSE` global: Mutex<MouseState> with x, y (start at 400, 300), buttons
- **Current status:** PS/2 AUX works in theory; QEMU on Windows SDL routes physical mouse through absolute pointer, not PS/2 AUX. Workaround: WASD keys move cursor.
- **Fix:** Implement XHCI + USB HID for proper USB mouse support

### nvme.rs
- Finds NVMe via PCI (class 0x01, subclass 0x08)
- Reads 64-bit BAR0; maps via `paging::map_mmio(bar_phys, 65536)`
- Queue depth: 64 entries each
- Init sequence: disable controller → setup admin queues (ASQ/ACQ/AQA) → enable → Identify Controller → create I/O CQ/SQ
- CC register: `(4 << 20) | (6 << 16) | 1` = IOCQES=4(16B), IOSQES=6(64B), EN=1
- `read_blocks(lba, count, phys)`, `write_blocks(lba, count, phys)` — polling (no interrupts)
- **LBA size hardcoded to 512** (QEMU default); full Identify Namespace hangs on QEMU
- **QEMU NVMe BAR:** 0xFEBD4000

### hepfs.rs — HepOS Filesystem

**Disk layout (4KB blocks):**
```
Block 0       : Superblock (magic, sizes, counts)
Block 1       : Inode bitmap (32,768 bits)
Blocks 2–5    : Block bitmap (131,072 bits = covers 512MB disk)
Blocks 6–37   : Inode table (32 blocks × 32 inodes/block = 1,024 inodes)
Blocks 38+    : Data blocks
```

**Inode (128 bytes):**
- `flags: u32` — 0=free, 1=file, 2=dir
- `size: u64` — bytes
- `nblocks: u32`
- `ctime, mtime: u64`
- `direct: [u32; 12]` — 12 direct block pointers (absolute block numbers)
- `indirect: u32` — single indirect (not yet used)
- Max file size: 12 × 4096 = 49,152 bytes (direct only)

**Directory entry (32 bytes):**
- `inode: u32` (0 = empty slot)
- `name_len: u8`
- `name: [u8; 27]` — max filename 27 chars

**Key functions:**
- `format(ctrl)` — writes superblock, clears bitmaps, creates root dir (inode 0)
- `probe(ctrl)` → bool — checks magic number
- `lookup(ctrl, "/path/to/file")` → `Option<u32>` — path resolution
- `create_file(ctrl, parent_ino, name)` → inode id
- `create_dir(ctrl, parent_ino, name)` → inode id
- `write_file(ctrl, ino, data: &[u8])`
- `read_file(ctrl, ino)` → `Vec<u8>`
- `list_dir(ctrl, ino)` → `Vec<(u32, String)>`

**MAGIC:** `0x48657046_53000001` ("HepFS\0\0\1")

**I/O:** Every read/write allocates a DMA page from PMM (HHDM-backed), does NVMe r/w, discards. No caching.

### desktop.rs — Desktop Environment

**Color palette:**
```
BG:          0x0D0D0D  (near-black)
ACCENT:      0x6C8EFF  (blue)
WIN_BG:      0x141414
WIN_TITLE:   0x1A1A2E  (inactive)
WIN_TITLE_A: 0x252550  (active)
TEXT:        0xE8E8E8
TASKBAR:     0x0A0A14
CLOSE_BTN:   0x8B1A1A  (dark red)
CURSOR:      0xFFFFFF
```

**Window struct:**
- id, title, x/y position, w/h size
- minimized, dragging state, drag offset
- No per-window content buffer (was too large for 1MB heap)
- Apps draw directly into the display via Desktop::render()

**Desktop struct:**
- `windows: Vec<Window>` — z-ordered (last = topmost)
- `focused: Option<usize>` — focused window id
- `dirty: bool` + `prev_cx/cy` — only re-renders when something changed
- `fb_w, fb_h` — framebuffer dimensions

**Window geometry:**
- Title bar: 22px tall
- Border: 1px
- Close button: 14×14px at top-right of title bar
- Taskbar: 32px at bottom

**Input handling (`update_mouse`):**
- Click on title → bring to front, start drag
- Click on close → minimize
- Click on taskbar button → toggle minimize/bring to front
- Drag moves window within screen bounds (clamped)

**Render order:**
1. `display.clear(BG)` — full screen dark background
2. Draw each non-minimized window (bottom to top)
3. Draw taskbar
4. Draw cursor crosshair (9×1 + 1×9 white lines)

**Dirty flag:** re-renders only when mouse moves, window dragged, or button clicked. Saves significant CPU.

**Render rate:** ~16ms delay loop in task_blink (≈60fps cap)

### Global State (main.rs)
```rust
pub static DISPLAY:  Mutex<Option<Display>>  = Mutex::new(None);
pub static DESKTOP:  Mutex<Option<Desktop>>  = Mutex::new(None);
pub static MOUSE:    Mutex<MouseState>       = ...;  // in mouse.rs
pub static SCHEDULER: Mutex<Scheduler>       = ...;  // in scheduler.rs
pub static HEAP:     BumpHeap                = ...;  // in heap.rs, #[global_allocator]
```

---

## Boot Sequence

```
Limine BIOS → kmain()
  1. serial::init()          COM1 debug output
  2. gdt::init()             Load our GDT
  3. idt::init()             Load IDT with exception handlers
  4. vmm::init(hhdm)         Store HHDM offset
  5. pmm::init(hhdm)         Parse Limine memory map, free pages >1MB
  6. heap::HEAP.init()       Allocate 256 PMM pages as bump heap
  7. Display::new(fb)        Wrap Limine framebuffer
  8. Draw splash screen      "HepOS / kernel alive / v0.1 / RAM: X MB"
  9. Desktop::new(w,h)       Create desktop with 3 initial windows
 10. DESKTOP = Some(dt)      Set global before enabling interrupts
 11. idt::set_handler(0x20)  Register APIC timer handler
 12. apic::init()            Enable x2APIC, start periodic timer
 13. Scheduler::add(idle)    Add idle task (hlt loop)
 14. Scheduler::add(blink)   Add desktop render task
 15. sti                     Enable interrupts — scheduler now runs
 16. pci::enumerate()        Find all PCI devices
 17. cli                     Disable interrupts for NVMe init
 18. nvme::init()            Initialize NVMe controller
 19. hepfs::probe/format     Mount or format HepFS
 20. hepfs smoke test        Create dirs/files, verify
 21. sti                     Re-enable interrupts
 22. ps2::init()             PS/2 keyboard init
 23. mouse::init()           PS/2 mouse init (with timeouts)
 24. loop { spin_loop }      kmain idle — task_blink does all work
```

---

## What's Built (✓) vs What's Left (○)

### Kernel
| Component | Status | Notes |
|-----------|--------|-------|
| Boot (Limine) | ✓ | BIOS, higher-half |
| Framebuffer + text | ✓ | GOP, 8×8 font, scaling |
| GDT | ✓ | No TSS yet |
| IDT + exceptions | ✓ | Red screen on fault |
| PMM | ✓ | Bitmap, >1MB only |
| HHDM/VMM | ✓ | phys↔virt conversion |
| Paging (map_mmio) | ✓ | For NVMe BAR |
| Heap | ✓ | Bump, 1MB, no-free |
| x2APIC + timer | ✓ | 10ms periodic |
| Preemptive scheduler | ✓ | Round-robin, 2 tasks |
| Syscall interface | ○ | SYSCALL/SYSRET needed for userspace |
| Per-process paging | ○ | Clone PML4 per process |
| TSS | ○ | Needed for ring 3 stack |
| Process manager | ○ | fork/exec primitives |

### Drivers
| Driver | Status | Notes |
|--------|--------|-------|
| PCI enumeration | ✓ | Config space I/O |
| PS/2 keyboard | ✓ | Scancode set 1, ASCII |
| PS/2 mouse | ✓ (driver) | QEMU doesn't route to PS/2 AUX; use WASD |
| NVMe | ✓ | Admin+IO queues, R/W |
| XHCI (USB 3.0) | ○ | Needed for USB mouse/keyboard on real HW |
| USB HID | ○ | After XHCI |
| Intel HDA (audio) | ○ | Complex codec enumeration |
| RTL8169 (Ethernet) | ○ | Your hardware's NIC |
| Intel e1000e | ○ | Common laptop/ThinkPad NIC |
| Intel i225/i226 | ○ | Modern desktop NIC |
| virtio-net | ○ | QEMU networking |
| ACPI | ○ | Shutdown/reboot/power |

### Networking
| Component | Status | Notes |
|-----------|--------|-------|
| smoltcp | ○ | Pure Rust no_std TCP/IP stack |
| NIC driver (one of above) | ○ | Prerequisite |
| DNS | ○ | After smoltcp |

### Storage / Filesystem
| Component | Status | Notes |
|-----------|--------|-------|
| HepFS format | ✓ | Inode table, bitmap |
| File CRUD | ✓ | create, read, write |
| Directory CRUD | ✓ | create, list, lookup |
| Path resolution | ✓ | `/a/b/c` walks |
| VFS layer | ○ | Abstract over multiple FS types |
| File deletion | ○ | bitmap free + inode clear |
| Large files | ○ | Indirect block support |
| Permissions | ○ | Not planned (single-user) |

### LibOS
| Component | Status | Notes |
|-----------|--------|-------|
| Kernel-space alloc | ✓ | Bump heap |
| Per-process allocator | ○ | User-space malloc |
| Scheduler policy | ✓ (basic) | Round-robin kernel-side |
| std shim | ○ | Allow `std`-using Rust crates |
| Symphonia shim | ○ | For audio playback |

### File Formats
| Format | Status |
|--------|--------|
| PNG/JPG | ○ (image crate, needs std shim) |
| MP3/FLAC/WAV | ○ (Symphonia, needs shim) |
| MP4/H.264 | ○ (openh264) |
| PDF | ○ (lopdf) |
| ZIP/TAR | ○ |
| Markdown | ○ |

### Desktop Environment
| Component | Status | Notes |
|-----------|--------|-------|
| Compositor (dirty rects) | ✓ | Re-renders on change |
| Floating window manager | ✓ | Drag, z-order, focus |
| Taskbar | ✓ | App buttons, minimize/restore |
| Mouse cursor | ✓ | Crosshair drawn by compositor |
| Physical mouse input | ○ | Blocked by QEMU PS/2 routing; fix with XHCI |
| Window animations | ○ | 150–200ms ease-out |
| Desktop icons | ○ | |
| Notifications | ○ | |

### Apps
| App | Status | Notes |
|-----|--------|-------|
| Terminal emulator | ○ | VT100 subset, highest priority |
| File manager | ○ | Two-pane, HepFS browser |
| Text editor | ○ | Syntax highlighting (Rust/C) |
| Image viewer | ○ | After PNG support |
| Audio/video player | ○ | After HDA + Symphonia |
| PDF/MD viewer | ○ | |
| Settings | ○ | Volume, maybe resolution |

---

## Recommended Build Order (Next Steps)

### Immediate (unblock interactive use)
1. **ACPI shutdown** — write 0x2000 to QEMU ACPI port 0x604; parse FADT for real HW
2. **Terminal emulator** — render text in Terminal window, keyboard input, scrolling
3. **File manager** — list HepFS root in HepFS window

### Short term (make it usable)
4. **File deletion** — free inode bitmap + data blocks
5. **Window content API** — let apps draw into window content area
6. **XHCI USB** — host controller, device enumeration
7. **USB HID** — keyboard + mouse via USB (real mouse movement!)

### Medium term (feature complete)
8. **std shim** — no_std stubs for `std::fs`, `std::io`, `std::thread` so Rust crates link
9. **Symphonia** — audio decoding (MP3/FLAC/WAV)
10. **Image crate** — PNG/JPG display
11. **RTL8169 NIC** — Ethernet for user's hardware
12. **smoltcp** — TCP/IP stack

### Long term (full OS)
13. Syscall interface (ring 3 userspace)
14. Per-process virtual memory
15. Audio player app
16. Web-adjacent features (maybe)

---

## Known Issues / Bugs

| Issue | Cause | Fix |
|-------|-------|-----|
| Physical mouse doesn't move | QEMU SDL routes mouse to absolute pointer, not PS/2 AUX | Implement XHCI + USB HID |
| Heap can't free memory | Bump allocator is no-free | Replace with slab allocator when needed |
| NVMe Identify Namespace hangs | QEMU NSID=1 CNS=0 command doesn't respond | Hardcoded lba_size=512 as workaround |
| No file deletion | Not implemented in HepFS | Add inode/block free bitmap operations |
| No large files >49KB | Only 12 direct blocks | Add indirect block support |
| Single-user only | No permission system | By design for now |

---

## Hardware Details (QEMU defaults)

| Device | Value |
|--------|-------|
| RAM | 256MB |
| NVMe disk | 512MB raw image (`hepos_disk.img`) |
| NVMe BAR phys | 0xFEBD4000 |
| LAPIC phys | 0xFEE00000 (accessed via x2APIC MSRs) |
| CAP register | 0x4008200F0107FF (DSTRD=0, TO=15, MQES=2047) |
| PCI devices | Host Bridge, VGA, RTL8139 Ethernet, NVMe, ISA Bridge, SATA, Unknown |
| HHDM offset | 0xFFFF800000000000 (typical) |

---

## Development Tips

- **Serial output** appears in the PowerShell window that launched build.ps1
- **Exception crashes** show a red screen with vector/error code/RIP
- **WASD** moves the cursor in QEMU (until USB mouse works)
- **Spacebar** = mouse click
- **Ctrl+Alt+G** in QEMU SDL = attempt to grab physical mouse (may not work on Windows)
- **Adding a new driver:** (1) add mod in main.rs, (2) call init() in boot sequence with CLI if touching MMIO, (3) add to PCI class check if PCI-based
- **Adding a new file to HepFS:** call `hepfs::create_file(ctrl, parent_ino, name)` then `hepfs::write_file(ctrl, ino, data)`
- **Heap exhaustion** shows as silent hangs (bump allocator returns null → alloc crate panics → spin)

---

## Crate Dependencies

```toml
limine = "0.6.5"   # Limine boot protocol bindings
spin   = "0.9"     # Mutex (spinlock-based, no_std)
# alloc built from source (rust-src component)
```

No other external dependencies. Everything else is written from scratch.

---

## GitHub

Repository: https://github.com/Aaaaaaajiandah/HepOS
Branch: master
Git user: aaaaaaajiandah
