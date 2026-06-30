//! VT100-subset terminal emulator.
//! Renders text into a window content area on the HepOS desktop.

use alloc::{vec::Vec, string::String};
use spin::Mutex;
use crate::{framebuffer::{Color, Display}, ps2};

// Terminal dimensions (in character cells) — scale 2 (18×18 px per cell)
const SCALE:      usize = 2;
pub const COLS:   usize = 30;
pub const ROWS:   usize = 10;
const SCROLLBACK: usize = 200;
const CHAR_W:     usize = 9 * SCALE + 1;   // 19 px per column
const CHAR_H:     usize = 8 * SCALE + 2;   // 18 px per row

// Palette
const TEXT:   Color = Color::from_hex(0xE8E8E8);
const DIM:    Color = Color::from_hex(0x888888);
const BG:     Color = Color::from_hex(0x0C0C0C);
const CURSOR: Color = Color::from_hex(0x6C8EFF);
const ERR:    Color = Color::from_hex(0xFF6B6B);
const OK:     Color = Color::from_hex(0x6BFF8E);

#[derive(Clone, Copy)]
struct Cell {
    ch:    u8,
    color: Color,
}

impl Cell {
    const fn blank() -> Self { Self { ch: b' ', color: TEXT } }
}

pub struct Terminal {
    lines:       Vec<[Cell; COLS]>,
    col:         usize,
    row:         usize,
    pub dirty:   bool,
    cmd_buf:     String,
    prompt_row:  usize,
    // Shell state
    cwd_ino:     u32,
    cwd_path:    String,
    // History
    history:     Vec<String>,
    history_idx: Option<usize>,
}

impl Terminal {
    pub fn new() -> Self {
        let mut lines = Vec::new();
        // Pre-fill with blank lines
        for _ in 0..SCROLLBACK {
            lines.push([Cell::blank(); COLS]);
        }
        let mut t = Terminal {
            lines,
            col: 0, row: 0, dirty: true,
            cmd_buf: String::new(), prompt_row: 0,
            cwd_ino: crate::hepfs::ROOT_INO,
            cwd_path: String::from("/"),
            history: Vec::new(),
            history_idx: None,
        };
        t.print_colored("HepOS Terminal v0.1\n", OK);
        t.print_colored("Type 'help' for commands\n\n", DIM);
        t.show_prompt();
        t
    }

    fn show_prompt(&mut self) {
        self.print_colored(&alloc::format!("{} $ ", self.cwd_path), CURSOR);
        self.prompt_row = self.row;
    }

    fn print_colored(&mut self, s: &str, color: Color) {
        for ch in s.chars() {
            self.put_char(ch as u8, color);
        }
        self.dirty = true;
    }

    fn print(&mut self, s: &str) {
        self.print_colored(s, TEXT);
    }

    fn put_char(&mut self, ch: u8, color: Color) {
        match ch {
            b'\n' => {
                self.col = 0;
                self.advance_row();
            }
            b'\r' => {
                self.col = 0;
            }
            _ if ch >= 32 => {
                if self.row < self.lines.len() {
                    self.lines[self.row][self.col] = Cell { ch, color };
                }
                self.col += 1;
                if self.col >= COLS {
                    self.col = 0;
                    self.advance_row();
                }
            }
            _ => {}
        }
    }

    fn advance_row(&mut self) {
        self.row += 1;
        if self.row >= SCROLLBACK {
            // Shift lines up
            self.lines.remove(0);
            self.lines.push([Cell::blank(); COLS]);
            self.row = SCROLLBACK - 1;
        }
    }

    /// Handle a keypress from PS/2.
    pub fn on_key(&mut self, c: char) {
        match c as u8 {
            b'\n' => {
                self.put_char(b'\n', TEXT);
                let cmd = alloc::string::String::from(self.cmd_buf.trim());
                self.cmd_buf.clear();
                self.history_idx = None;
                if !cmd.is_empty() {
                    self.history.push(cmd.clone());
                    if self.history.len() > 50 { self.history.remove(0); }
                }
                self.execute(&cmd);
                self.show_prompt();
            }
            b'\x08' => { // backspace
                if !self.cmd_buf.is_empty() {
                    self.cmd_buf.pop();
                    if self.col > 0 { self.col -= 1; }
                    self.lines[self.row][self.col] = Cell::blank();
                }
            }
            b'\x03' => { // Ctrl+C — cancel current input
                self.cmd_buf.clear();
                self.print_colored("^C\n", ERR);
                self.show_prompt();
            }
            b'\x0C' | b'\x0B' => { // Ctrl+L or Ctrl+K = clear
                for line in &mut self.lines { *line = [Cell::blank(); COLS]; }
                self.col = 0; self.row = 0;
                self.show_prompt();
            }
            ps2::KEY_UP | 0x10 => { // UP arrow or Ctrl+P — history previous
                if self.history.is_empty() { self.dirty = true; return; }
                let new_idx = match self.history_idx {
                    None    => self.history.len() - 1,
                    Some(0) => 0,
                    Some(i) => i - 1,
                };
                self.history_idx = Some(new_idx);
                let entry = self.history[new_idx].clone();
                self.replace_input(&entry);
            }
            ps2::KEY_DOWN | 0x0E => { // DOWN arrow or Ctrl+N — history next
                let action = match self.history_idx {
                    None => None,
                    Some(i) if i + 1 >= self.history.len() => Some(None),
                    Some(i) => Some(Some(i + 1)),
                };
                match action {
                    None => {}
                    Some(None) => { self.history_idx = None; self.replace_input(""); }
                    Some(Some(i)) => {
                        self.history_idx = Some(i);
                        let entry = self.history[i].clone();
                        self.replace_input(&entry);
                    }
                }
            }
            ch if ch >= 32 => {
                if self.cmd_buf.len() < COLS - 2 {
                    self.cmd_buf.push(ch as char);
                    self.put_char(ch, TEXT);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn replace_input(&mut self, new: &str) {
        // Erase current input on this row back to prompt end
        while self.col > 0 && self.cmd_buf.len() > 0 {
            self.cmd_buf.pop();
            self.col -= 1;
            self.lines[self.row][self.col] = Cell::blank();
        }
        // Also handle if cmd_buf had more chars than shown
        self.cmd_buf.clear();
        // Redraw from prompt start
        for ch in new.bytes() {
            self.cmd_buf.push(ch as char);
            self.put_char(ch, TEXT);
        }
    }

    fn execute(&mut self, cmd: &str) {
        let parts: alloc::vec::Vec<&str> = cmd.splitn(3, ' ').collect();
        let verb = parts.first().copied().unwrap_or("");
        let arg1 = parts.get(1).copied().unwrap_or("");
        let arg2 = parts.get(2).copied().unwrap_or("");

        match verb {
            "" => {}

            "help" => {
                self.print_colored("Commands:\n", OK);
                let cmds = [
                    ("help",           "this message"),
                    ("clear",          "clear screen"),
                    ("pwd",            "print working directory"),
                    ("ls [path]",      "list directory"),
                    ("cd <dir>",       "change directory"),
                    ("cat <file>",     "print file contents"),
                    ("mkdir <name>",   "create directory"),
                    ("touch <name>",   "create empty file"),
                    ("rm <name>",      "remove file or empty dir"),
                    ("write <f> <txt>","write text to file"),
                    ("uname",          "system info"),
                    ("mem",            "memory usage"),
                    ("shutdown",       "power off (ACPI)"),
                    ("reboot",         "reboot"),
                    ("echo <text>",    "print text"),
                ];
                for (name, desc) in &cmds {
                    self.print_colored("  ", DIM);
                    self.print_colored(name, TEXT);
                    self.print_colored(" - ", DIM);
                    self.print(desc);
                    self.print("\n");
                }
            }

            "clear" => {
                for line in &mut self.lines { *line = [Cell::blank(); COLS]; }
                self.col = 0; self.row = 0;
            }

            "pwd" => {
                self.print_colored(&self.cwd_path.clone(), OK);
                self.print("\n");
            }

            "ls" => {
                let target_ino = if arg1.is_empty() {
                    Some(self.cwd_ino)
                } else {
                    self.resolve(arg1)
                };
                match target_ino {
                    None => { self.print_colored("ls: not found\n", ERR); }
                    Some(ino) => {
                        let entries = self.with_ctrl(|ctrl| crate::hepfs::list_dir(ctrl, ino));
                        if entries.is_empty() {
                            self.print_colored("(empty)\n", DIM);
                        }
                        for (child_ino, name) in entries {
                            let (is_dir, sz) = self.with_ctrl(|ctrl| crate::hepfs::stat(ctrl, child_ino));
                            if is_dir {
                                self.print_colored(&alloc::format!("{}/\n", name), CURSOR);
                            } else {
                                self.print_colored(&name, TEXT);
                                self.print_colored(&alloc::format!("  ({} B)\n", sz), DIM);
                            }
                        }
                    }
                }
            }

            "cd" => {
                if arg1 == "/" {
                    self.cwd_ino  = crate::hepfs::ROOT_INO;
                    self.cwd_path = String::from("/");
                } else if arg1 == ".." {
                    // Go up — re-resolve parent from path
                    let parent_path = {
                        let p = self.cwd_path.trim_end_matches('/');
                        match p.rfind('/') {
                            Some(0) | None => String::from("/"),
                            Some(i)        => String::from(&p[..i]),
                        }
                    };
                    let new_ino = self.with_ctrl(|ctrl|
                        crate::hepfs::lookup(ctrl, &parent_path)
                    ).unwrap_or(crate::hepfs::ROOT_INO);
                    self.cwd_ino  = new_ino;
                    self.cwd_path = parent_path;
                } else {
                    match self.resolve(arg1) {
                        None => { self.print_colored("cd: not found\n", ERR); }
                        Some(ino) => {
                            let (is_dir, _) = self.with_ctrl(|ctrl| crate::hepfs::stat(ctrl, ino));
                            if !is_dir {
                                self.print_colored("cd: not a directory\n", ERR);
                            } else {
                                self.cwd_ino = ino;
                                self.cwd_path = if self.cwd_path == "/" {
                                    alloc::format!("/{}", arg1)
                                } else {
                                    alloc::format!("{}/{}", self.cwd_path, arg1)
                                };
                            }
                        }
                    }
                }
            }

            "cat" => {
                if arg1.is_empty() { self.print_colored("usage: cat <file>\n", ERR); return; }
                match self.resolve(arg1) {
                    None => { self.print_colored("cat: not found\n", ERR); }
                    Some(ino) => {
                        let data = self.with_ctrl(|ctrl| crate::hepfs::read_file(ctrl, ino));
                        let s = alloc::string::String::from_utf8_lossy(&data);
                        self.print(&s);
                        if !s.ends_with('\n') { self.print("\n"); }
                    }
                }
            }

            "mkdir" => {
                if arg1.is_empty() { self.print_colored("usage: mkdir <name>\n", ERR); return; }
                let cwd = self.cwd_ino;
                self.with_ctrl(|ctrl| { crate::hepfs::create_dir(ctrl, cwd, arg1); });
                self.print_colored("created\n", OK);
            }

            "touch" => {
                if arg1.is_empty() { self.print_colored("usage: touch <name>\n", ERR); return; }
                let cwd = self.cwd_ino;
                self.with_ctrl(|ctrl| { crate::hepfs::create_file(ctrl, cwd, arg1); });
                self.print_colored("created\n", OK);
            }

            "rm" => {
                if arg1.is_empty() { self.print_colored("usage: rm <name>\n", ERR); return; }
                let cwd = self.cwd_ino;
                let ok = self.with_ctrl(|ctrl| crate::hepfs::remove(ctrl, cwd, arg1));
                if ok { self.print_colored("removed\n", OK); }
                else  { self.print_colored("rm: failed (not found or dir not empty)\n", ERR); }
            }

            "write" => {
                if arg1.is_empty() || arg2.is_empty() {
                    self.print_colored("usage: write <file> <content>\n", ERR); return;
                }
                let cwd = self.cwd_ino;
                let ino = match self.resolve(arg1) {
                    Some(i) => i,
                    None    => self.with_ctrl(|ctrl| crate::hepfs::create_file(ctrl, cwd, arg1)),
                };
                let data = arg2.as_bytes();
                self.with_ctrl(|ctrl| crate::hepfs::write_file(ctrl, ino, data));
                self.print_colored("written\n", OK);
            }

            "history" => {
                let hist: alloc::vec::Vec<String> = self.history.clone();
                for (i, h) in hist.iter().enumerate() {
                    self.print_colored(&alloc::format!("{:3}  ", i + 1), DIM);
                    self.print(h);
                    self.print("\n");
                }
            }

            "date" => {
                let mut tb = [0u8; 6];
                let mut db = [0u8; 11];
                let time = crate::rtc::fmt_time(&mut tb);
                let date = crate::rtc::fmt_date(&mut db);
                self.print_colored(date, TEXT);
                self.print("  ");
                self.print_colored(time, CURSOR);
                self.print("\n");
            }

            "sysinfo" => {
                self.print_colored("=== HepOS Kernel Info ===\n", OK);
                self.print_colored("Kernel:    ", DIM); self.print("HepOS v0.1\n");
                self.print_colored("Arch:      ", DIM); self.print("x86_64\n");
                self.print_colored("Type:      ", DIM); self.print("Exokernel (Rust)\n");
                self.print_colored("Boot:      ", DIM); self.print("Limine v9 (BIOS)\n");
                self.print_colored("Language:  ", DIM); self.print("Rust (no_std + alloc)\n");
                self.print_colored("Heap:      ", DIM); self.print("Bump allocator 1MB\n");
                self.print_colored("Sched:     ", DIM); self.print("Preemptive round-robin\n");
                self.print_colored("APIC:      ", DIM); self.print("x2APIC (MSR-mode)\n");
                self.print_colored("FS:        ", DIM); self.print("HepFS (flat inode, 4KB blocks)\n");
                self.print_colored("Display:   ", DIM); self.print("GOP framebuffer, software render\n");
                self.print_colored("Storage:   ", DIM); self.print("NVMe (custom driver)\n");
                let free_mb  = crate::pmm::free_pages() * 4 / 1024;
                let total_mb = crate::pmm::total_pages() * 4 / 1024;
                self.print_colored("RAM:       ", DIM);
                self.print_u64(free_mb); self.print(" MB free / ");
                self.print_u64(total_mb); self.print(" MB total\n");
                self.print_colored("=========================\n", DIM);
                self.print_colored("Source: github.com/The-Hep-Group/HepOS\n", DIM);
            }

            "uname" => {
                self.print_colored("HepOS", CURSOR);
                self.print(" v0.1  x86_64 exokernel  HepFS\n");
            }

            "mem" => {
                let free  = crate::pmm::free_pages() * 4;
                let total = crate::pmm::total_pages() * 4;
                self.print_colored("RAM: ", DIM);
                self.print_u64(free);
                self.print(" KB free / ");
                self.print_u64(total);
                self.print(" KB total\n");
            }

            "netpoll" => {
                // Directly scan all 8 RX descriptors and show status
                self.print_colored("Scanning RX descriptors...\n", DIM);
                {
                    let nic_guard = crate::e1000::NIC.lock();
                    if let Some(nic) = nic_guard.as_ref() {
                        for i in 0..8usize {
                            let st  = nic.rx_status(i);
                            let len = nic.rx_len(i);
                            self.print(&alloc::format!("  desc[{}]: status={:02X} len={}\n",
                                i, st, len));
                        }
                    } else {
                        self.print_colored("NIC is None - run netstart first\n", ERR);
                    }
                }
            }

            "netstart" => {
                // Force-initialize e1000 from terminal (bar at 0xFEBC0000 from lspci)
                self.print_colored("Starting e1000...\n", DIM);
                crate::e1000::force_init(0xFEBC_0000);
                if crate::e1000::NIC.lock().is_some() {
                    self.print_colored("NIC initialized! Try ping 10.0.2.2\n", OK);
                    crate::net::arp_announce();
                } else {
                    self.print_colored("NIC still None - check serial\n", ERR);
                }
            }

            "netdiag" => {
                // Read e1000 PCI config and registers directly (bus=0,dev=3,func=0)
                let bus = 0u8; let dev = 3u8; let func = 0u8;
                let vid = crate::pci::config_read16(bus, dev, func, 0x00);
                let did = crate::pci::config_read16(bus, dev, func, 0x02);
                let cmd = crate::pci::config_read16(bus, dev, func, 0x04);
                let bar0 = crate::pci::config_read32(bus, dev, func, 0x10);
                let bar1 = crate::pci::config_read32(bus, dev, func, 0x14);
                self.print(&alloc::format!("00:03.0  VID:{:04X} DID:{:04X}\n", vid, did));
                self.print(&alloc::format!("CMD: {:04X}\n", cmd));
                self.print(&alloc::format!("BAR0: {:08X}  BAR1: {:08X}\n", bar0, bar1));

                // Compute BAR physical address
                let bar_phys = if (bar0 & 6) == 4 {
                    ((bar1 as u64) << 32) | ((bar0 & !0xF) as u64)
                } else {
                    (bar0 & !0xF) as u64
                };
                self.print(&alloc::format!("BAR phys: {:016X}\n", bar_phys));

                if bar_phys != 0 {
                    // Map and read CTRL + STATUS
                    let regs = crate::paging::map_mmio(bar_phys, 131072);
                    let ctrl   = unsafe { (regs as *const u32).read_volatile() };
                    let status = unsafe { (regs.add(8) as *const u32).read_volatile() };
                    let ral    = unsafe { (regs.add(0x5400) as *const u32).read_volatile() };
                    let rah    = unsafe { (regs.add(0x5404) as *const u32).read_volatile() };
                    self.print(&alloc::format!("CTRL:   {:08X}\n", ctrl));
                    self.print(&alloc::format!("STATUS: {:08X}\n", status));
                    self.print(&alloc::format!("RAL:    {:08X}  RAH: {:08X}\n", ral, rah));
                    let mac = [
                        (ral & 0xFF) as u8, (ral >> 8 & 0xFF) as u8,
                        (ral >> 16 & 0xFF) as u8, (ral >> 24) as u8,
                        (rah & 0xFF) as u8, (rah >> 8 & 0xFF) as u8,
                    ];
                    self.print(&alloc::format!(
                        "MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}\n",
                        mac[0],mac[1],mac[2],mac[3],mac[4],mac[5]));
                    self.print(&alloc::format!("e1000::NIC is {}\n",
                        if crate::e1000::NIC.lock().is_some() { "SOME" } else { "NONE" }));
                    // Read back TX registers to verify init
                    let tctl   = unsafe { (regs.add(0x400) as *const u32).read_volatile() };
                    let tdbal  = unsafe { (regs.add(0x3800) as *const u32).read_volatile() };
                    let tdlen  = unsafe { (regs.add(0x3808) as *const u32).read_volatile() };
                    let tdh    = unsafe { (regs.add(0x3810) as *const u32).read_volatile() };
                    let tdt    = unsafe { (regs.add(0x3818) as *const u32).read_volatile() };
                    self.print(&alloc::format!("TCTL:{:08X} TDBAL:{:08X} TDLEN:{:04X} TDH:{} TDT:{}\n",
                        tctl, tdbal, tdlen, tdh, tdt));
                } else {
                    self.print_colored("BAR phys = 0, device not initialized by BIOS\n", ERR);
                }
            }

            "lspci" => {
                let devs = crate::PCI_DEVS.lock();
                for d in devs.iter() {
                    self.print(&alloc::format!(
                        "{:02X}:{:02X}.{} {:04X}:{:04X} {}\n",
                        d.bus, d.dev, d.func,
                        d.vendor_id, d.device_id,
                        crate::pci::class_name(d.class, d.subclass)
                    ));
                }
            }

            "ifconfig" => {
                let has_nic = crate::e1000::NIC.lock().is_some();
                let mac = crate::e1000::NIC.lock()
                    .as_ref().map(|n| n.mac).unwrap_or([0;6]);
                if !has_nic {
                    self.print_colored("eth0: NIC not initialized\n", ERR);
                    self.print_colored("      (check serial for e1000 init messages)\n", DIM);
                }
                let ip = crate::net::MY_IP;
                let gw = crate::net::GW_IP;
                self.print_colored("eth0\n", OK);
                self.print_colored("  MAC: ", DIM);
                self.print(&alloc::format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}\n",
                    mac[0],mac[1],mac[2],mac[3],mac[4],mac[5]));
                self.print_colored("  IP:  ", DIM);
                self.print(&alloc::format!("{}.{}.{}.{}\n", ip[0],ip[1],ip[2],ip[3]));
                self.print_colored("  GW:  ", DIM);
                self.print(&alloc::format!("{}.{}.{}.{}\n", gw[0],gw[1],gw[2],gw[3]));
                self.print_colored("  Net: ", DIM);
                self.print("255.255.255.0\n");
            }

            "ping" => {
                if arg1.is_empty() {
                    self.print_colored("usage: ping <ip>\n", ERR);
                } else {
                    let parts: alloc::vec::Vec<&str> = arg1.split('.').collect();
                    if parts.len() == 4 {
                        let ip: [u8; 4] = [
                            parts[0].parse().unwrap_or(0),
                            parts[1].parse().unwrap_or(0),
                            parts[2].parse().unwrap_or(0),
                            parts[3].parse().unwrap_or(0),
                        ];
                        self.print_colored(&alloc::format!("PING {}\n", arg1), DIM);
                        let result = crate::net::ping(ip);
                        self.print(&result);
                        self.print("\n");
                    } else {
                        self.print_colored("ping: invalid IP address\n", ERR);
                    }
                }
            }

            "edit" => {
                if arg1.is_empty() {
                    self.print_colored("usage: edit <file>\n", ERR);
                } else {
                    let full = if arg1.starts_with('/') {
                        String::from(arg1)
                    } else if self.cwd_path == "/" {
                        alloc::format!("/{}", arg1)
                    } else {
                        alloc::format!("{}/{}", self.cwd_path, arg1)
                    };
                    crate::editor::open(&full);
                    // Un-minimize editor window (id=3), bring to front, focus it
                    {
                        let mut dt = crate::desktop::DESKTOP.lock();
                        if let Some(dt) = dt.as_mut() {
                            if let Some(w) = dt.windows.iter_mut().find(|w| w.id == 3) {
                                w.minimized = false;
                                w.title = alloc::format!("Editor: {}", arg1);
                            }
                            dt.bring_to_front(3);
                            dt.dirty = true;
                        }
                    }
                    *crate::FOCUSED_WIN.lock() = Some(3);
                    self.print_colored("Editor opened  Ctrl+S=save  Ctrl+Q=close\n", OK);
                }
            }

            "shutdown" => { self.print_colored("Shutting down...\n", ERR); crate::acpi::shutdown(); }
            "reboot"   => { self.print_colored("Rebooting...\n",    ERR); crate::acpi::reboot(); }

            s if s.starts_with("echo") => {
                self.print(if arg1.is_empty() { "\n" } else { &cmd[5..] });
                self.print("\n");
            }

            other => {
                self.print_colored(other, ERR);
                self.print_colored(": command not found  (try 'help')\n", DIM);
            }
        }
    }

    /// Resolve a name relative to cwd (or absolute path).
    fn resolve(&mut self, name: &str) -> Option<u32> {
        if name.starts_with('/') {
            self.with_ctrl(|ctrl| crate::hepfs::lookup(ctrl, name))
        } else {
            let cwd = self.cwd_ino;
            self.with_ctrl(|ctrl| {
                crate::hepfs::list_dir(ctrl, cwd)
                    .into_iter()
                    .find(|(_, n)| n.as_str() == name)
                    .map(|(ino, _)| ino)
            })
        }
    }

    /// Run a closure with the global NVMe controller locked.
    fn with_ctrl<T, F: FnOnce(&mut crate::nvme::NvmeController) -> T>(&self, f: F) -> T {
        let mut guard = crate::nvme::CONTROLLER.lock();
        f(guard.as_mut().expect("no NVMe controller"))
    }

    fn print_u64(&mut self, mut n: u64) {
        if n == 0 { self.put_char(b'0', TEXT); return; }
        let mut buf = [0u8; 20];
        let mut i = 20usize;
        while n > 0 {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        for &b in &buf[i..] {
            self.put_char(b, TEXT);
        }
    }

    /// Render terminal content into the window content area.
    pub fn render(&self, display: &mut Display, wx: usize, wy: usize, ww: usize, wh: usize) {
        // Fill background
        display.fill_rect(wx, wy, ww, wh, BG);

        // Accent line at top so we can see the render is happening
        display.fill_rect(wx, wy, ww, 2, CURSOR);

        let visible_rows = (wh.saturating_sub(6)) / CHAR_H;
        let start = if self.row + 1 >= visible_rows {
            self.row + 1 - visible_rows
        } else {
            0
        };

        for r in 0..visible_rows {
            let line_idx = start + r;
            if line_idx >= self.lines.len() { break; }
            let line = &self.lines[line_idx];
            let py = wy + 4 + r * CHAR_H;
            if py + CHAR_H > wy + wh { break; }

            for (ci, cell) in line.iter().enumerate() {
                let px = wx + 4 + ci * CHAR_W;
                if px + CHAR_W > wx + ww { break; }
                if cell.ch > b' ' {
                    let s = core::str::from_utf8(
                        core::slice::from_ref(&cell.ch)
                    ).unwrap_or("?");
                    display.draw_text(px, py, s, cell.color, SCALE);
                }
            }
        }

        // Cursor underline
        let vis_row = self.row.saturating_sub(start).min(visible_rows.saturating_sub(1));
        let cx = wx + 4 + self.col * CHAR_W;
        let cy = wy + 4 + vis_row * CHAR_H + CHAR_H - 2;
        if cx + 16 <= wx + ww && cy + 2 <= wy + wh {
            display.fill_rect(cx, cy, 16, 2, CURSOR);
        }
    }
}

pub static TERMINAL: Mutex<Option<Terminal>> = Mutex::new(None);

pub fn init() {
    *TERMINAL.lock() = Some(Terminal::new());
    // Trigger desktop re-render so terminal content appears immediately
    if let Some(dt) = crate::desktop::DESKTOP.lock().as_mut() {
        dt.dirty = true;
    }
}
