# HepOS — Complete Design Reference

> **Purpose:** Authoritative reference for HepOS. Survives context compaction.
> **Last updated:** 2026-06-30

---

## Overview

HepOS is a custom x86\_64 operating system written in Rust using an **exokernel architecture**. The kernel only does hardware multiplexing; all OS abstractions live in a kernel-space libOS for now. Single user, no permissions, networking partially implemented.

**Language:** Rust (nightly, `no_std` + `alloc`)  
**Target:** x86\_64, bare metal  
**Bootloader:** Limine v9.x (BIOS mode)  
**Dev machine:** Windows 11, QEMU 11.x  
**License:** MIT  
**Repository:** https://github.com/The-Hep-Group/HepOS

---

## Source Files

```
kernel/src/
  main.rs        kmain entry, global state, task setup, window+HepFS rendering
  framebuffer.rs GOP pixel/rect/text renderer (8×8 bitmap font, LSB-first)
  gdt.rs         GDT (null, code64, data64)
  idt.rs         IDT, exception stubs, timer_stub
  pmm.rs         Bitmap PMM (pages above 1MB only, alloc_contiguous)
  vmm.rs         HHDM offset, phys_to_virt
  paging.rs      PML4 walker, map_page, map_mmio (NOCACHE)
  heap.rs        Bump allocator 1MB, GlobalAlloc, no-free
  apic.rs        x2APIC (MSR), 10ms timer, disables 8259 PIC
  acpi.rs        ACPI shutdown (port 0x604) + PS/2 reboot
  rtc.rs         CMOS RTC: now(), fmt_time(), fmt_date()
  scheduler.rs   Round-robin preemptive, context_switch (naked asm)
  pci.rs         Config-space scan (0xCF8/0xCFC), enumerate()
  ps2.rs         PS/2 kbd: scancode set 1 + extended (0xE0) + shift/caps/ctrl/PgUp/PgDn
  mouse.rs       PS/2 AUX mouse (3-byte packets, relative, AUX port)
  xhci.rs        XHCI USB host controller — USB HID tablet, absolute mouse coords
  nvme.rs        NVMe driver, admin+IO queues, global CONTROLLER
  hepfs.rs       HepFS: flat inode, 4KB blocks, NVMe backend
  desktop.rs     Compositor, WM, start menu, taskbar, resize handles, RTC clock
  terminal.rs    Full shell: pwd/cd/ls/cat/mkdir/rm/cp/mv/write/edit/ping/tab-complete/...
  editor.rs      Text editor: arrow nav, PgUp/Dn, Ctrl+Home/End, F2=save, F10=close
  net.rs         ARP, ICMP, eth_send, ping (bypasses ARP, uses SLiRP MAC)
  e1000.rs       Intel 82540EM driver (TX works, RX pending)
  rtl8139.rs     RTL8139 driver (flat ring, TX works, RX broken on QEMU Windows)
  virtio_net.rs  virtio-net legacy (incomplete)
  serial.rs      COM1 debug: print, print_hex
  panic.rs       Prints file:line:message to serial, then spins
```

---

## QEMU Command

```powershell
qemu-system-x86_64
  -M q35
  -cpu qemu64,+x2apic      # x2APIC via MSR
  -m 256M
  -cdrom hepos.iso
  -boot d
  -drive file=hepos_disk.img,if=none,id=nvme0,format=raw
  -device nvme,serial=heposv1,drive=nvme0
  -netdev user,id=net0
  -device rtl8139,netdev=net0
  -device qemu-xhci,id=xhci         # XHCI USB host controller
  -device usb-tablet,bus=xhci.0     # absolute mouse via USB HID
  -vga std
  -display sdl,window-close=off
  -serial stdio
  -no-reboot
  -no-shutdown
```

---

## Boot Sequence

```
Limine → kmain()
 1. serial, GDT, IDT
 2. VMM (HHDM offset), PMM (pages >1MB)
 3. Heap (bump, 256 PMM pages = 1MB)
 4. Display + splash screen
 5. Desktop + windows created (before sti)
 6. Terminal init + HepFS navigator state (before sti)
 7. PCI enumerate
 8. NVMe init → HepFS mount/format → write /kernel.txt
 9. Networking init (RTL8139 → e1000 → virtio fallbacks)
10. PS/2 keyboard + mouse init
11. XHCI USB init (finds usb-tablet, sets up HID ring)
12. Scheduler (2 tasks: idle, task_blink) + APIC timer
13. sti → first timer tick switches to task_blink
14. task_blink loops forever (input poll + render)
```

**Critical ordering:** APIC timer and scheduler MUST start last, after all device init.
The first timer tick context-switches kmain → task_blink. If APIC starts early,
task_blink runs before XHCI/networking are initialized.

---

## Focus System

- **Default:** Terminal focused (`FOCUSED_WIN = Some(2)`), all keys → terminal
- **ESC:** Enter cursor mode (`FOCUSED_WIN = None`, yellow crosshair, WASD moves cursor)
- **ESC from editor:** close editor window → terminal focus
- **Space on window (cursor mode):** focus that window
- **Mouse click on window:** brings it to front AND syncs keyboard focus to it
- **Ctrl+C in terminal:** cancel current input, show `^C`

---

## Desktop Windows (IDs)

| ID | Title | Content |
|----|-------|---------|
| 0 | Welcome to HepOS | System info, RAM, NVMe/HepFS status |
| 1 | HepFS | File manager with directory navigation, back/forward, path bar |
| 2 | Terminal | Full interactive shell with tab completion |
| 3 | Editor | Text editor (opens with `edit <file>` or clicking a file in HepFS) |

All windows support:
- **Drag** title bar to move
- **Drag** bottom-right resize handle to resize (min 120×60)
- **Close** (×) button to minimize to taskbar

---

## Taskbar & Start Menu

- **Start button** (left): opens a popup listing ALL programs regardless of state
  - Click any program to un-minimize and focus it
  - Minimized programs show `--` badge
- **Window buttons** (right of start): only OPEN (non-minimized) windows shown
  - Click focused window's button to minimize it
  - Click another window's button to focus it
- **Clock** (far right): live RTC time

---

## Terminal Shell Commands

| Command | Description |
|---------|-------------|
| `help` | List all commands |
| `pwd` | Print working directory |
| `ls [path]` | List directory |
| `cd <dir>` | Change dir (`..` and `/` supported) |
| `cat <file>` | Print file contents |
| `mkdir <name>` | Create directory |
| `touch <name>` | Create empty file |
| `rm <name>` | Remove file or empty directory |
| `cp <src> <dst>` | Copy file |
| `mv <src> <dst>` | Move / rename file |
| `write <file> <text>` | Write text to file |
| `edit <file>` | Open text editor |
| `history` | Show command history |
| `↑/↓` arrows | Navigate history (Ctrl+P/N also work) |
| `Tab` | Tab completion (commands and filenames) |
| `date` | Current date+time (RTC) |
| `sysinfo` | Full kernel info |
| `uname` / `mem` | System info / memory usage |
| `lspci` | List all PCI devices with vendor:device IDs |
| `netdiag` | e1000 register dump |
| `netstart` | Force-init e1000 NIC |
| `netpoll` | Scan RX descriptors |
| `ifconfig` | Show IP/MAC/GW |
| `ping <ip>` | ICMP echo (hardcoded SLiRP MAC) |
| `shutdown` / `reboot` | ACPI power off / PS/2 reset |
| `echo <text>` / `clear` | Print text / clear screen |
| `Ctrl+L` / `Ctrl+K` | Clear screen |
| `Ctrl+A` / `Ctrl+E` | Jump to start/end of input |
| `Ctrl+C` | Cancel current input |

### Tab Completion Details
- **No space yet:** completes command name (`cat<Tab>` → `cat `)
- **After space:** completes filename from cwd (`cat he<Tab>` → `cat hello.txt `)
- **Single match:** auto-completes in place with trailing space
- **Multiple matches:** prints all options on a new line, re-shows prompt + partial input

---

## Text Editor Controls

| Key | Action |
|-----|--------|
| Arrow keys | Move cursor |
| Home / End | Line start / end |
| Ctrl+Home | Jump to file start |
| Ctrl+End | Jump to file end |
| Page Up / Page Down | Scroll one screen |
| Enter | Insert newline (splits line) |
| Backspace / Delete | Delete character |
| Tab | Insert 4 spaces |
| F2 / Ctrl+S | Save file |
| F10 / Ctrl+Q / ESC | Close (warns if unsaved) |

Display: line numbers, current-line highlight, cursor underline, file name + modified indicator, line:col status bar.

---

## HepFS File Manager Window

- **Nav bar** at top: `[<] [>] /path/here`
  - `<` back: navigate to previous directory
  - `>` forward: navigate forward after going back
  - Path bar shows current path (truncated from left if too long)
- **File list:**
  - `d` prefix (blue) for directories, `f` prefix (white) for files
  - File sizes shown on the right
  - `..` entry at top when not in root — click to go up
- **Click a directory** → navigate into it (pushes history)
- **Click a file** → opens it in the editor window

---

## HepFS Layout

```
Block 0      : Superblock (magic 0x48657046_53000001)
Block 1      : Inode bitmap
Blocks 2-5   : Block bitmap
Blocks 6-37  : Inode table (1024 inodes × 128 bytes)
Blocks 38+   : Data blocks (4KB each)
```
Max file size: 12 × 4KB = 49KB (direct blocks only)  
`/kernel.txt` written at every boot (kernel manifest)

---

## XHCI USB Mouse Driver

**Device:** QEMU `qemu-xhci` (PCI 1B36:000D) + `usb-tablet` (USB HID, absolute coordinates)

**USB HID report format (6 bytes):**
```
[0] buttons   [1] x_lo  [2] x_hi  [3] y_lo  [4] y_hi  [5] wheel
```
Absolute range: 0–32767 → scaled to framebuffer resolution.

**Key implementation details:**
- Link TRB TC bit must ALWAYS be 1 (not just on odd wraps) — fixes ring desync after 2nd wrap
- Filter out `(x=0, y=0, buttons=0)` reports — initial garbage before mouse enters QEMU window
- Port speed read from PORTSC bits[13:10] after reset (don't hardcode USB2)
- Sequence: Enable Slot → Address Device → SET_CONFIGURATION → Configure Endpoint → queue HID TRBs → poll

---

## Networking Status

**Architecture:** Ethernet → ARP/IP → ICMP  
**Stack:** hand-written (no smoltcp), in `net.rs`  
**Static IP:** 10.0.2.15, GW: 10.0.2.2, mask: 255.255.255.0

| Driver | Status | Notes |
|--------|--------|-------|
| virtio-net | ✗ not found | PCI vendor 0x1AF4 not detected in QEMU |
| e1000 (82540EM) | ⚠ TX only | TDH advances, correct data, SLiRP not responding on Win |
| rtl8139 | ⚠ TX only | Simpler flat ring, same QEMU Windows SLiRP issue |

**Root cause:** QEMU on Windows with SLiRP — TX is correct (MAC/size verified) but packets never reach SLiRP. Works on Linux/KVM. Not a driver bug.

**Workaround:** `netstart` force-inits e1000. `ping 10.0.2.2` shows "timeout" (TX works, RX broken).

---

## What's Built ✓ vs Left ○

### Kernel
| Status | Feature |
|--------|---------|
| ✓ | Boot/Limine, Framebuffer, GDT, IDT |
| ✓ | PMM (>1MB only), HHDM, Paging |
| ✓ | Bump heap, x2APIC, ACPI, RTC |
| ✓ | Preemptive scheduler, PCI enumeration |
| ✓ | Panic handler prints file:line:message to serial |
| ○ | Syscall interface (SYSCALL/SYSRET) |
| ○ | Per-process paging, TSS |
| ○ | Slab allocator (bump heap cannot free) |

### Input / Drivers
| Status | Feature |
|--------|---------|
| ✓ | PS/2 keyboard (shift, caps, ctrl, arrows, F-keys, PgUp/PgDn) |
| ✓ | PS/2 mouse (relative, driver works; QEMU SDL doesn't route to AUX) |
| ✓ | XHCI USB host controller + USB HID tablet (absolute mouse coords) |
| ✓ | NVMe (admin+IO queues, hardcoded 512B block size) |
| ✓ | PCI enumeration |
| ○ | Intel HDA audio |
| ○ | RTL8169 (user's real NIC on hardware) |
| ○ | ACPI full (FADT parsing for real hardware — currently hardcoded QEMU port) |

### Storage
| Status | Feature |
|--------|---------|
| ✓ | HepFS: format, probe, create/read/write/delete files+dirs |
| ✓ | Path resolution, kernel manifest (`/kernel.txt`) |
| ✓ | `cp` / `mv` commands |
| ○ | Large files >49KB (indirect blocks needed) |
| ○ | VFS layer |

### Desktop / WM
| Status | Feature |
|--------|---------|
| ✓ | Compositor with correct z-order (chrome + content rendered together per window) |
| ✓ | Floating WM: drag to move, close button (minimizes to taskbar) |
| ✓ | Window resize: drag bottom-right corner, min 120×60 |
| ✓ | Start menu (all programs), taskbar shows only open windows |
| ✓ | Focus system: mouse click syncs both visual + keyboard focus |
| ✓ | Live RTC clock on taskbar |
| ○ | Desktop icons / wallpaper |
| ○ | Multiple instances of same app |
| ○ | Window maximize / snap |

### Apps
| Status | Feature |
|--------|---------|
| ✓ | Terminal: full shell, history, arrow nav, Ctrl+C/L, tab completion |
| ✓ | Text editor: arrow nav, PgUp/Dn, Ctrl+Home/End, F2=save, F10=close |
| ✓ | HepFS file manager: directory navigation, back/forward/path bar, click-to-open |
| ✓ | Welcome window: system info, RAM, NVMe/HepFS status |
| ○ | Ctrl+F find/replace in editor |
| ○ | Multiple terminal windows |
| ○ | Settings / system monitor window |
| ○ | Image viewer (needs std shim) |

### Networking
| Status | Feature |
|--------|---------|
| ✓ | ARP, ICMP, IP checksum, eth_send |
| ✓ | e1000 TX works (TDH advances, data correct) |
| ⚠ | RX broken on QEMU Windows (SLiRP issue, not driver bug) |
| ○ | Working ping end-to-end |
| ○ | TCP/UDP stack |
| ○ | DNS, HTTP client |

### LibOS / Ecosystem
| Status | Feature |
|--------|---------|
| ○ | `std` shim → unlock Rust crates (image, audio, etc.) |
| ○ | PNG/JPG rendering (needs std shim) |
| ○ | MP3/FLAC audio via Symphonia (needs std shim + HDA) |
| ○ | Userspace (ring 3, syscalls, ELF loader) |

---

## Known Issues

| Issue | Cause | Fix |
|-------|-------|-----|
| Networking RX broken on QEMU Win | SLiRP/QEMU Windows path — not a driver bug | Test on Linux/KVM |
| Heap can't free | Bump allocator by design | Slab allocator (future) |
| NVMe reports 0 MB | Identify Namespace CMD hangs | Hardcoded 512B workaround |
| Files max 49KB | Only 12 direct blocks in inode | Add indirect block pointer |
| ACPI only on QEMU | Hardcoded port 0x604 (QEMU PIIX4) | Parse FADT for real hardware |
| Terminal wraps at 30 cols | COLS=30 constant | Reflow on window resize |

---

## Next Steps (Priority Order)

1. **Ctrl+F find in editor** — search forward through file, highlight match, Ctrl+G next
2. **Terminal reflow on resize** — recalculate COLS from window width so terminal uses full width
3. **RTL8139 / networking** — test on Linux/KVM; if confirmed working there, QEMU Windows is a known non-issue
4. **Indirect blocks in HepFS** — one extra `indirect` pointer per inode unlocks files up to ~4MB
5. **Settings / sysmon window** — real-time RAM graph, uptime, connected PCI devices list
6. **std shim** — implement enough of `std` (alloc, io, fs) to link external Rust crates
7. **Intel HDA audio** — enumerate HDA via PCI, play PCM samples; pair with a simple beep/music app
8. **Userspace / ring 3** — SYSCALL/SYSRET gate, per-process page tables, ELF loader, basic shell process
9. **Slab allocator** — replace bump heap; needed before userspace can fork/spawn reasonably
10. **RTL8169 NIC** — for use on real hardware (user's actual NIC)

---

## Key Global State (main.rs)

```rust
pub static DISPLAY:      Mutex<Option<Display>>          // GOP framebuffer
pub static FOCUSED_WIN:  Mutex<Option<usize>>            // None=cursor mode, Some(id)=focused window
pub static PCI_DEVS:     Mutex<Vec<PciDevice>>           // for lspci command
static     HEPFS_NAV:    Mutex<Option<HepfsNav>>         // HepFS window navigator state
// In other modules:
desktop::DESKTOP         Mutex<Option<Desktop>>          // window manager, z-order, focus
nvme::CONTROLLER         Mutex<Option<NvmeController>>   // global NVMe
e1000::NIC               Mutex<Option<E1000>>
rtl8139::NIC             Mutex<Option<Rtl8139>>
terminal::TERMINAL       Mutex<Option<Terminal>>
editor::EDITOR           Mutex<Option<Editor>>
xhci::XHCI_STATE        (module-internal, polled each frame)
scheduler::SCHEDULER     Mutex<Scheduler>
mouse::MOUSE             Mutex<Mouse>                    // shared x/y/buttons
```

### HepfsNav (in main.rs)
```rust
struct HepfsNav {
    ino:  u32,          // current directory inode
    path: String,       // display path e.g. "/home"
    back: Vec<(u32, String)>,   // back navigation stack
    fwd:  Vec<(u32, String)>,   // forward navigation stack
}
```

---

## Render Loop (task_blink)

Runs forever, renders at ~60fps when dirty:
```
1.  poll ps2 + mouse + XHCI (sets mouse::MOUSE x/y/buttons)
2.  route keys:
      ESC (not editor) → cursor mode (FOCUSED_WIN = None)
      focused=Some(3)  → editor.on_key()
      focused=Some(_)  → terminal.on_key()
      None (cursor)    → WASD move cursor, Space = click
3.  clamp cursor to framebuffer bounds, write back to mouse::MOUSE
4.  update_mouse(mx, my, btn):
      handles drag, resize, taskbar, start menu, window clicks
5.  sync FOCUSED_WIN ← desktop.focused on fresh mouse click
6.  HepFS window click handler (nav buttons, dir nav, file→editor)
7.  if dirty:
      a. clear background (desktop.render)
      b. for each non-minimized window in z-order (bottom→top):
           draw_window chrome (border, title bar, content bg)
           draw window content (render_welcome / render_hepfs / terminal.render / editor.render)
      c. draw_start_menu (if open)
      d. draw_taskbar (always on top)
      e. draw cursor (yellow crosshair = cursor mode, white cross = window focused)
8.  spin ~16ms
```

---

## Terminal Render Constants

```rust
const SCALE:      usize = 2;   // 2× font scale for readability
const COLS:       usize = 30;  // characters per line (fixed — doesn't reflow yet)
const SCROLLBACK: usize = 200; // scrollback buffer lines
const CHAR_W:     usize = 19;  // pixels per column (9 * SCALE + 1)
const CHAR_H:     usize = 18;  // pixels per row    (8 * SCALE + 2)
```

---

## Architecture Notes

- **PMM only frees pages above 1MB:** avoids 0xA0000–0xFFFFF reserved hole (VGA/BIOS)
- **Bump heap assumes contiguous pages:** first 256 PMM pages must be contiguous (true above 1MB on QEMU)
- **Scheduler starts last:** APIC timer + scheduler init happens after ALL device init. First timer tick switches kmain → task_blink with IF=0; task_blink enables interrupts via spin_loop.
- **x2APIC (MSR mode):** xAPIC MMIO at 0xFEE00000 isn't in Limine HHDM; MSRs bypass this
- **PS/2 poll ordering:** `ps2::poll()` must run before `mouse::poll()` or mouse bytes get eaten
- **XHCI ring wrap:** Link TRB TC bit must be 1 on EVERY wrap (not just odd ones). Otherwise XHC doesn't toggle PCS → desync → transfers stop after 2nd wrap.
- **QEMU SDL mouse:** SDL routes the pointer to USB tablet (absolute), not PS/2 AUX. XHCI handles this correctly.
- **Z-order rendering:** Windows vec order = z-order. `bring_to_front` removes + re-appends. Chrome + content rendered together per window (not all chrome then all content) to prevent lower-window content painting over upper-window chrome.

---

## Dev Tips

- **Serial output** → PowerShell terminal running build.ps1 (early boot messages scroll past quickly)
- **Mouse** → move freely after XHCI init; no WASD needed unless in cursor mode
- **ESC** → cursor mode (yellow `+` crosshair); click a window or press Space to re-focus
- **Tab in terminal** → complete command name or filename
- **Resize a window** → drag the bottom-right corner (handle shown as diagonal dots)
- **HepFS navigation** → click `<` / `>` buttons or click a directory entry; click `..` to go up
- **Click a file in HepFS** → opens in editor; editor auto-focuses
- **Ctrl+L / Ctrl+K** in terminal → clear screen
- **↑/↓** → history; Ctrl+P/N = reliable alternative
- **F2** = editor save; **F10** = editor close (warns if unsaved, second press force-closes)
- **PgUp/PgDn** in editor → scroll one screen; **Ctrl+Home/End** → file start/end
- **`netstart`** → manually init e1000 (workaround for auto-init panic)
- **`lspci`** → see all PCI devices with vendor:device IDs
- **`sysinfo`** / **`cat /kernel.txt`** → kernel info from inside OS
- **Panic output** → now printed to serial (file:line:message) before spinning

---

## QEMU Hardware Details

| Item | Value |
|------|-------|
| RAM | 256MB |
| NVMe | 512MB raw (`hepos_disk.img`, auto-created if missing) |
| NVMe BAR | 0xFEBD4000 |
| e1000 BAR | 0xFEBC0000 |
| e1000 MAC | 52:54:00:12:34:56 |
| SLiRP gateway | 10.0.2.2, MAC 52:55:0a:00:02:02 |
| HHDM offset | 0xFFFF800000000000 (typical Limine value) |
| XHCI | PCI 1B36:000D (qemu-xhci), usb-tablet on bus xhci.0 |

---

## Crate Dependencies

```toml
limine = "0.6.5"   # MIT
spin   = "0.9"     # MIT
# alloc from rust-src (MIT/Apache-2)
```

All drivers, FS, networking, and desktop written from scratch.
