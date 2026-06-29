//! VT100-subset terminal emulator.
//! Renders text into a window content area on the HepOS desktop.

use alloc::{vec::Vec, string::String};
use spin::Mutex;
use crate::framebuffer::{Color, Display};

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
    // Ring buffer of lines
    lines:      Vec<[Cell; COLS]>,
    col:        usize,     // cursor column
    row:        usize,     // cursor row (0 = top visible)
    pub dirty:  bool,
    // Simple command buffer for "shell"
    cmd_buf:    String,
    prompt_row: usize,     // row where the current prompt is
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
            col: 0,
            row: 0,
            dirty: true,
            cmd_buf: String::new(),
            prompt_row: 0,
        };
        t.print_colored("HepOS Terminal v0.1\n", OK);
        t.print_colored("Type 'help' for commands\n\n", DIM);
        t.show_prompt();
        t
    }

    fn show_prompt(&mut self) {
        self.print_colored("hepos> ", CURSOR);
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
        match c {
            '\n' => {
                self.put_char(b'\n', TEXT);
                let cmd = alloc::string::String::from(self.cmd_buf.trim());
                self.cmd_buf.clear();
                self.execute(&cmd);
                self.show_prompt();
            }
            '\x08' => { // backspace
                if !self.cmd_buf.is_empty() {
                    self.cmd_buf.pop();
                    // Erase character on screen
                    if self.col > 0 {
                        self.col -= 1;
                    }
                    self.lines[self.row][self.col] = Cell::blank();
                }
            }
            c if (c as u32) >= 32 => {
                if self.cmd_buf.len() < COLS - 10 {
                    self.cmd_buf.push(c);
                    self.put_char(c as u8, TEXT);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn execute(&mut self, cmd: &str) {
        match cmd {
            "" => {}

            "help" => {
                self.print_colored("Commands:\n", OK);
                self.print("  help     - this message\n");
                self.print("  clear    - clear screen\n");
                self.print("  ls       - list / directory\n");
                self.print("  uname    - system info\n");
                self.print("  mem      - memory usage\n");
                self.print("  shutdown - power off\n");
                self.print("  reboot   - reboot\n");
                self.print("  echo ... - print text\n");
            }

            "clear" => {
                for line in &mut self.lines {
                    *line = [Cell::blank(); COLS];
                }
                self.col = 0;
                self.row = 0;
            }

            "uname" => {
                self.print_colored("HepOS", CURSOR);
                self.print(" v0.1 x86_64 exokernel\n");
            }

            "mem" => {
                let free  = crate::pmm::free_pages()  * 4;
                let total = crate::pmm::total_pages() * 4;
                self.print_colored("RAM: ", DIM);
                self.print_u64(free);
                self.print(" KB free / ");
                self.print_u64(total);
                self.print(" KB total\n");
            }

            "ls" => {
                // Show the HepFS root contents if available
                self.print_colored("/\n", OK);
                self.print("  home/\n");
                self.print("  etc/\n");
                self.print("  (HepFS mounted)\n");
            }

            "shutdown" => {
                self.print_colored("Shutting down...\n", ERR);
                crate::acpi::shutdown();
            }

            "reboot" => {
                self.print_colored("Rebooting...\n", ERR);
                crate::acpi::reboot();
            }

            s if s.starts_with("echo ") => {
                self.print(&s[5..]);
                self.print("\n");
            }

            other => {
                self.print_colored("Unknown command: ", ERR);
                self.print(other);
                self.print("\n");
                self.print_colored("Type 'help'\n", DIM);
            }
        }
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
