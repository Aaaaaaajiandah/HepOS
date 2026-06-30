#![no_std]
#![no_main]
#![feature(stmt_expr_attributes)]
extern crate alloc;

mod acpi;
mod apic;
mod editor;
mod e1000;
mod net;
mod rtc;
mod rtl8139;
mod virtio_net;
mod desktop;
mod framebuffer;
mod gdt;
mod heap;
mod hepfs;
mod idt;
mod mouse;
mod nvme;
mod paging;
mod panic;
mod pci;
mod pmm;
mod ps2;
mod scheduler;
mod serial;
mod terminal;
mod vmm;

use framebuffer::Display;
use limine::request::{FramebufferRequest, HhdmRequest};
use limine::BaseRevision;
use spin::Mutex;

// Global display — used by exception handler and future modules
pub static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);

// Focus: None = cursor mode (WASD moves cursor), Some(id) = window has keyboard focus
pub static FOCUSED_WIN: Mutex<Option<usize>> = Mutex::new(None);
pub static PCI_DEVS: Mutex<alloc::vec::Vec<pci::PciDevice>> = Mutex::new(alloc::vec::Vec::new());

#[used] static BASE_REVISION:       BaseRevision       = BaseRevision::new();
#[used] static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();
#[used] static HHDM_REQUEST:        HhdmRequest        = HhdmRequest::new();

#[no_mangle]
extern "C" fn kmain() -> ! {
    serial::init();
    serial::print("HepOS kmain\n");

    gdt::init();
    serial::print("GDT loaded\n");

    idt::init();
    serial::print("IDT loaded\n");

    let hhdm = HHDM_REQUEST.response().expect("no HHDM").offset;
    vmm::init(hhdm);
    pmm::init(hhdm);
    serial::print("PMM init\n");

    heap::HEAP.init();
    serial::print("Heap init\n");

    // smoke test: allocate and use a Vec
    {
        use alloc::vec::Vec;
        let mut v: Vec<u32> = Vec::new();
        for i in 0..16 { v.push(i); }
        serial::print("Heap smoke test OK\n");
        let _ = v;
    }

    let fb = FRAMEBUFFER_REQUEST
        .response()
        .and_then(|r| r.framebuffers().first().copied())
        .expect("no framebuffer");

    *DISPLAY.lock() = Some(Display::new(fb));

    {
        let mut guard = DISPLAY.lock();
        let display = guard.as_mut().unwrap();

        display.clear(framebuffer::Color::from_hex(0x0D0D0D));

        let accent = framebuffer::Color::from_hex(0x6C8EFF);
        let white  = framebuffer::Color::from_hex(0xE8E8E8);
        let dim    = framebuffer::Color::from_hex(0x555555);

        display.fill_rect(0, 0, display.width(), 2, accent);

        let x_mid = display.width() / 2;
        let y_mid = display.height() / 2;

        display.draw_text(x_mid - 72, y_mid - 24, "HepOS",               accent, 3);
        display.draw_text(x_mid - 88, y_mid + 16, "kernel alive",         white,  2);
        display.draw_text(x_mid - 96, y_mid + 48, "v0.1 | x86_64 exokernel", dim, 1);

        // show memory stats
        let free_mb  = pmm::free_pages()  * 4 / 1024;
        let total_mb = pmm::total_pages() * 4 / 1024;
        let mut buf = [0u8; 64];
        let mem_str = fmt_mem(free_mb, total_mb, &mut buf);
        display.draw_text(x_mid - (mem_str.len() * 9 / 2), y_mid + 72, mem_str, dim, 1);
    }

    // Init desktop BEFORE enabling interrupts so task_blink sees it immediately
    {
        let fb = FRAMEBUFFER_REQUEST.response()
            .and_then(|r| r.framebuffers().first().copied())
            .expect("no framebuffer for desktop");
        let w = fb.width as usize;
        let h = fb.height as usize;
        let mut dt = desktop::Desktop::new(w, h);
        // Window positions chosen to fit common resolutions (640×480 min)
        dt.add_window("Welcome to HepOS", 20,  50,  300, 160);
        dt.add_window("HepFS",            340, 50,  260, 160);
        dt.add_window("Terminal",         20,  240, 580, 200);
        // Editor window (id=3) — hidden until `edit` command opens a file
        dt.add_window("Editor",           60,  40,  580, 380);
        *desktop::DESKTOP.lock() = Some(dt);
    }

    // Init terminal NOW before sti so task_blink sees it immediately
    terminal::init();
    serial::print("Terminal init\n");

    // Minimize editor until a file is opened; focus terminal (id=2)
    {
        let mut dt = desktop::DESKTOP.lock();
        if let Some(dt) = dt.as_mut() {
            if let Some(w) = dt.windows.iter_mut().find(|w| w.id == 3) {
                w.minimized = true;
            }
        }
    }
    *FOCUSED_WIN.lock() = Some(2);

    // Wire timer interrupt into IDT
    idt::set_handler(apic::timer_vector(), idt::timer_stub as u64);

    apic::init();
    serial::print("APIC init\n");

    // Add two tasks so the scheduler has something to switch between
    {
        let mut sched = scheduler::SCHEDULER.lock();
        sched.add(scheduler::Task::new(0, "idle",  task_idle));
        sched.add(scheduler::Task::new(1, "blink", task_blink));
        sched.tasks[0].state = scheduler::TaskState::Running;
    }

    serial::print("Scheduler ready\n");

    // Enable interrupts â€” APIC timer will now fire
    unsafe { core::arch::asm!("sti", options(nomem, nostack)); }

    // PCI enumeration
    let pci_devices = pci::enumerate();
    // Store for lspci command
    *PCI_DEVS.lock() = pci_devices.clone();
    serial::print("PCI devices:\n");
    for d in &pci_devices {
        serial::print("  ");
        serial::print(pci::class_name(d.class, d.subclass));
        serial::print("\n");
    }

    // Disable interrupts for entire NVMe + FS init â€” the timer firing mid-MMIO
    // causes a triple fault because the scheduler switches context before NVMe
    // queue setup is stable.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)); }

    // NVMe
    if let Some(mut ctrl) = nvme::init(&pci_devices) {
        serial::print("NVMe ready\n");
        let s = alloc::format!(
            "NVMe: {} MB  ({} byte blocks)\n",
            ctrl.lba_count * ctrl.lba_size as u64 / 1024 / 1024,
            ctrl.lba_size
        );
        serial::print(&s);
        // smoke test: write then read block 0
        let (phys, virt) = {
            let p = pmm::alloc_page().unwrap();
            (p, vmm::phys_to_virt(p))
        };
        unsafe { core::ptr::write_bytes(virt, 0xAB, 512); }
        ctrl.write_blocks(0, 1, phys);
        unsafe { core::ptr::write_bytes(virt, 0x00, 512); }
        ctrl.read_blocks(0, 1, phys);
        let ok = unsafe { *(virt as *const u8) } == 0xAB;
        serial::print(if ok { "NVMe R/W OK\n" } else { "NVMe R/W FAIL\n" });

        // HepFS
        if !hepfs::probe(&mut ctrl) {
            serial::print("Formatting HepFS...\n");
            hepfs::format(&mut ctrl);
            serial::print("HepFS formatted\n");
        } else {
            serial::print("HepFS found\n");
        }

        // Smoke test: create dirs + file, write, read back
        hepfs::create_dir(&mut ctrl, hepfs::ROOT_INO, "home");
        hepfs::create_dir(&mut ctrl, hepfs::ROOT_INO, "etc");
        let home = hepfs::lookup(&mut ctrl, "/home").unwrap();
        let fno  = hepfs::create_file(&mut ctrl, home, "hello.txt");
        hepfs::write_file(&mut ctrl, fno, b"Hello from HepOS!\n");
        let data = hepfs::read_file(&mut ctrl, fno);
        let s    = core::str::from_utf8(&data).unwrap_or("?");
        serial::print("Read back: ");
        serial::print(s);

        let entries = hepfs::list_dir(&mut ctrl, hepfs::ROOT_INO);
        serial::print("/ contents:\n");
        for (_, name) in &entries { serial::print("  "); serial::print(name); serial::print("\n"); }

        // Store controller globally so apps can use it
        *nvme::CONTROLLER.lock() = Some(ctrl);

        // Write kernel manifest to HepFS
        {
            let mut c = nvme::CONTROLLER.lock();
            if let Some(ctrl) = c.as_mut() {
                // Create /kernel.txt if it doesn't exist
                if hepfs::lookup(ctrl, "/kernel.txt").is_none() {
                    let ino = hepfs::create_file(ctrl, hepfs::ROOT_INO, "kernel.txt");
                    let mut db = [0u8; 11];
                    let date = rtc::fmt_date(&mut db);
                    let content = alloc::format!(
                        "HepOS Kernel Manifest\n\
                         =====================\n\
                         Version:  v0.1\n\
                         Arch:     x86_64\n\
                         Type:     Exokernel (Rust no_std)\n\
                         Bootloader: Limine v9\n\
                         Date:     {}\n\
                         Repo:     github.com/The-Hep-Group/HepOS\n\
                         License:  MIT\n\
                         \n\
                         Subsystems:\n\
                           PMM (bitmap, >1MB pages)\n\
                           Heap (bump allocator)\n\
                           Paging (HHDM + map_mmio)\n\
                           x2APIC timer (10ms tick)\n\
                           Preemptive scheduler\n\
                           HepFS (flat inode, 4KB blocks)\n\
                           NVMe driver\n\
                           PS/2 keyboard+mouse\n\
                           GOP framebuffer compositor\n\
                           ACPI shutdown\n",
                        date
                    );
                    hepfs::write_file(ctrl, ino, content.as_bytes());
                }
            }
        }

    } else {
        serial::print("No NVMe device found\n");
    }

    // Re-enable interrupts now that NVMe + FS are stable
    unsafe { core::arch::asm!("sti", options(nomem, nostack)); }

    // Networking — try RTL8139 first (simplest QEMU NIC), then e1000
    rtl8139::init(&pci_devices);
    if rtl8139::NIC.lock().is_none() { e1000::init(&pci_devices); }
    net::arp_announce();
    serial::print("Network init\n");

    // Input devices
    ps2::init();
    mouse::init();
    serial::print("Input init\n");

    serial::print("Boot complete\n");

    loop { core::hint::spin_loop(); }
}

fn task_idle() -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
}

fn task_blink() -> ! {
    let mut mx: i32 = 400;
    let mut my: i32 = 300;
    let mut btn: u8  = 0;

    loop {
        ps2::poll();
        mouse::poll();

        // PS/2 mouse updates (relative movement from mouse::poll)
        {
            let m = mouse::MOUSE.lock();
            mx = m.x;
            my = m.y;
            btn = m.buttons;
        }

        // Keyboard routing depends on focus
        let mut ps2_had_input = false;
        while let Some(c) = ps2::read_char() {
            ps2_had_input = true;
            let focused = *FOCUSED_WIN.lock();

            match c {
                '\x1b' if focused != Some(3) => {
                    // ESC → cursor mode (yellow crosshair, WASD to move)
                    // Minimize editor window so it doesn't block clicks
                    {
                        let mut dt = desktop::DESKTOP.lock();
                        if let Some(dt) = dt.as_mut() {
                            if let Some(w) = dt.windows.iter_mut().find(|w| w.id == 3) {
                                w.minimized = true;
                            }
                            dt.dirty = true;
                        }
                    }
                    *FOCUSED_WIN.lock() = None;
                }
                _ if focused == Some(3) => {
                    // Editor has focus — route all keys including ESC
                    let mut eg = editor::EDITOR.lock();
                    if let Some(ed) = eg.as_mut() {
                        ed.on_key(c);
                        if !ed.open {
                            drop(eg);
                            let mut dt = desktop::DESKTOP.lock();
                            if let Some(dt) = dt.as_mut() {
                                if let Some(w) = dt.windows.iter_mut().find(|w| w.id == 3) {
                                    w.minimized = true;
                                }
                                dt.dirty = true;
                            }
                            *FOCUSED_WIN.lock() = Some(2);
                        }
                    }
                }
                _ if focused.is_some() => {
                    // Terminal (or other window) has focus
                    let mut tg = terminal::TERMINAL.lock();
                    if let Some(t) = tg.as_mut() { t.on_key(c); }
                }
                // Cursor mode (no focus)
                'w' => my -= 6,
                's' => my += 6,
                'a' => mx -= 6,
                'd' => mx += 6,
                ' ' => {
                    // Space = click: focus the topmost window under cursor
                    // Also check title bar area
                    let clicked_id = {
                        let dt = desktop::DESKTOP.lock();
                        dt.as_ref().and_then(|d| {
                            d.windows.iter().rev()
                                .find(|w| !w.minimized &&
                                    (w.content_hit(mx, my) || w.title_hit(mx, my)))
                                .map(|w| w.id)
                        })
                    };
                    if let Some(id) = clicked_id {
                        *FOCUSED_WIN.lock() = Some(id);
                        // Also un-minimize editor if clicking on it
                        if id == 3 {
                            let mut eg = editor::EDITOR.lock();
                            if let Some(ed) = eg.as_mut() {
                                if !ed.open { drop(eg); *FOCUSED_WIN.lock() = Some(2); }
                            }
                        }
                        let mut dt = desktop::DESKTOP.lock();
                        if let Some(dt) = dt.as_mut() { dt.dirty = true; }
                    }
                    // If nothing clicked, focus terminal as default
                    if FOCUSED_WIN.lock().is_none() {
                        *FOCUSED_WIN.lock() = Some(2);
                    }
                }
                _ => {}
            }
        }

        // Clamp and write back mouse state
        let (fb_w, fb_h) = {
            let dt = desktop::DESKTOP.lock();
            dt.as_ref().map(|d| (d.fb_w as i32, d.fb_h as i32)).unwrap_or((1280, 720))
        };
        mx = mx.clamp(0, fb_w - 1);
        my = my.clamp(0, fb_h - 1);
        {
            let mut m = mouse::MOUSE.lock();
            m.x = mx; m.y = my; m.buttons = btn;
        }

        // Update WM (update_mouse sets dirty flag if position changed)
        {
            let mut dt_guard = desktop::DESKTOP.lock();
            if let Some(dt) = dt_guard.as_mut() {
                dt.update_mouse(mx, my, btn);
            }
        }

        // Also force dirty after keyboard input so terminal text appears
        if ps2_had_input {
            let mut dt = desktop::DESKTOP.lock();
            if let Some(dt) = dt.as_mut() { dt.dirty = true; }
            let mut tm = terminal::TERMINAL.lock();
            if let Some(tm) = tm.as_mut() { tm.dirty = true; }
        }

        // Render desktop + terminal every frame when dirty
        let desktop_dirty = desktop::DESKTOP.lock().as_ref().map(|d| d.dirty).unwrap_or(false);
        let term_dirty    = terminal::TERMINAL.lock().as_ref().map(|t| t.dirty).unwrap_or(false);

        if desktop_dirty || term_dirty || ps2_had_input {
            if let Some(display) = DISPLAY.lock().as_mut() {
                // 1. Render desktop
                {
                    let mut dt = desktop::DESKTOP.lock();
                    if let Some(dt) = dt.as_mut() {
                        dt.dirty = false;
                        dt.render(display, mx, my);
                    }
                }

                // 2-5. Render all window content IN Z-ORDER so topmost window always wins
                let win_order: alloc::vec::Vec<(usize, i32, i32, usize, usize)> = {
                    let dt = desktop::DESKTOP.lock();
                    dt.as_ref().map(|d| d.windows.iter()
                        .filter(|w| !w.minimized)
                        .map(|w| (w.id, w.x, w.y, w.w, w.h))
                        .collect()
                    ).unwrap_or_default()
                };

                for (id, wx, wy, ww, wh) in win_order {
                    let wx = wx.max(0) as usize;
                    let wy = wy.max(0) as usize;
                    match id {
                        0 => render_welcome_window(display),
                        1 => render_hepfs_window(display),
                        2 => {
                            let mut tg = terminal::TERMINAL.lock();
                            if let Some(t) = tg.as_mut() {
                                t.render(display, wx, wy, ww, wh);
                                t.dirty = false;
                            }
                        }
                        3 => {
                            let mut eg = editor::EDITOR.lock();
                            if let Some(ed) = eg.as_mut() {
                                ed.render(display, wx, wy, ww, wh);
                            }
                        }
                        _ => {}
                    }
                }

                // 5. Cursor mode indicator — yellow crosshair when unfocused
                {
                    let focused = *FOCUSED_WIN.lock();
                    if focused.is_none() {
                        let cx = mx as usize;
                        let cy = my as usize;
                        // Yellow crosshair = cursor mode
                        let col = framebuffer::Color::from_hex(0xFFFF00);
                        display.fill_rect(cx.saturating_sub(8), cy, 17, 1, col);
                        display.fill_rect(cx, cy.saturating_sub(8), 1, 17, col);
                    }
                }
            }
        }

        // ~60fps
        for _ in 0..1_200_000 { core::hint::spin_loop(); }
    }
}

fn window_rect(title: &str) -> Option<(usize, usize, usize, usize)> {
    let dt = desktop::DESKTOP.lock();
    dt.as_ref().and_then(|d| {
        d.windows.iter()
            .find(|w| !w.minimized && w.title.as_str() == title)
            .map(|w| (w.x.max(0) as usize, w.y.max(0) as usize, w.w, w.h))
    })
}

fn render_hepfs_window(display: &mut framebuffer::Display) {
    let Some((wx, wy, ww, wh)) = window_rect("HepFS") else { return; };
    let bg   = framebuffer::Color::from_hex(0x0C0C0C);
    let acc  = framebuffer::Color::from_hex(0x6C8EFF);
    let text = framebuffer::Color::from_hex(0xE8E8E8);
    let dim  = framebuffer::Color::from_hex(0x888888);

    display.fill_rect(wx, wy, ww, wh, bg);
    display.fill_rect(wx, wy, ww, 2, acc);
    display.draw_text(wx + 4, wy + 6, "/ (root)", acc, 1);

    let mut ctrl = nvme::CONTROLLER.lock();
    if let Some(ctrl) = ctrl.as_mut() {
        let entries = hepfs::list_dir(ctrl, hepfs::ROOT_INO);
        let mut y = wy + 22;
        for (ino, name) in &entries {
            if y + 14 > wy + wh { break; }
            let inode = hepfs::read_inode(ctrl, *ino);
            let prefix = if inode.flags == hepfs::F_DIR { "d " } else { "f " };
            display.draw_text(wx + 4, y, prefix, dim, 1);
            display.draw_text(wx + 22, y, name, text, 1);
            y += 14;
        }
        if entries.is_empty() {
            display.draw_text(wx + 4, wy + 22, "(empty)", dim, 1);
        }
    } else {
        display.draw_text(wx + 4, wy + 22, "No NVMe", dim, 1);
    }
}

fn render_welcome_window(display: &mut framebuffer::Display) {
    let Some((wx, wy, ww, wh)) = window_rect("Welcome to HepOS") else { return; };
    let bg   = framebuffer::Color::from_hex(0x0C0C0C);
    let acc  = framebuffer::Color::from_hex(0x6C8EFF);
    let text = framebuffer::Color::from_hex(0xE8E8E8);
    let dim  = framebuffer::Color::from_hex(0x888888);
    let ok   = framebuffer::Color::from_hex(0x6BFF8E);

    display.fill_rect(wx, wy, ww, wh, bg);
    display.fill_rect(wx, wy, ww, 2, acc);

    let mut y = wy + 6;
    display.draw_text(wx + 4, y, "HepOS v0.1", acc, 1);   y += 16;
    display.draw_text(wx + 4, y, "x86_64 exokernel", dim, 1); y += 14;

    let free_mb  = pmm::free_pages() * 4 / 1024;
    let total_mb = pmm::total_pages() * 4 / 1024;
    let mut buf = [0u8; 64];
    let s = fmt_mem(free_mb, total_mb, &mut buf);
    display.draw_text(wx + 4, y, s, text, 1); y += 14;

    let has_nvme = nvme::CONTROLLER.lock().is_some();
    display.draw_text(wx + 4, y, if has_nvme { "NVMe: OK" } else { "NVMe: --" },
        if has_nvme { ok } else { dim }, 1); y += 14;

    display.draw_text(wx + 4, y, "HepFS: OK", ok, 1); y += 14;
    display.draw_text(wx + 4, y, "ESC=cursor/terminal toggle", dim, 1);
    let _ = ww; let _ = wh;
}

fn fmt_mem<'a>(free_mb: u64, total_mb: u64, buf: &'a mut [u8; 64]) -> &'a str {
    let mut pos = 0usize;
    for b in b"RAM: "       { if pos < 64 { buf[pos] = *b; pos += 1; } }
    write_u64(free_mb,  &mut pos, buf);
    for b in b" MB free / " { if pos < 64 { buf[pos] = *b; pos += 1; } }
    write_u64(total_mb, &mut pos, buf);
    for b in b" MB total"   { if pos < 64 { buf[pos] = *b; pos += 1; } }
    core::str::from_utf8(&buf[..pos]).unwrap_or("")
}

fn write_u64(mut n: u64, pos: &mut usize, buf: &mut [u8; 64]) {
    if n == 0 { if *pos < 64 { buf[*pos] = b'0'; *pos += 1; } return; }
    let start = *pos;
    while n > 0 {
        if *pos < 64 { buf[*pos] = b'0' + (n % 10) as u8; *pos += 1; }
        n /= 10;
    }
    buf[start..*pos].reverse();
}
