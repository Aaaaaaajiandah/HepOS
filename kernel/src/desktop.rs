//! Desktop Environment — compositor, window manager, taskbar.
//!
//! All rendering is software-only into a back-buffer; dirty rects are
//! blitted to the real GOP framebuffer once per frame (~60 fps from the
//! APIC timer).  The WM uses a simple top-down Z-order list.

use alloc::{string::String, vec::Vec};
use spin::Mutex;
use crate::framebuffer::{Color, Display};

// ── Palette ─────────────────────────────────────────────────────────────────
pub mod pal {
    use crate::framebuffer::Color;
    pub const BG:          Color = Color::from_hex(0x0D0D0D);
    pub const ACCENT:      Color = Color::from_hex(0x6C8EFF);
    pub const WIN_BG:      Color = Color::from_hex(0x141414);
    pub const WIN_TITLE:   Color = Color::from_hex(0x1A1A2E);
    pub const WIN_TITLE_A: Color = Color::from_hex(0x252550); // active
    pub const TEXT:        Color = Color::from_hex(0xE8E8E8);
    pub const TEXT_DIM:    Color = Color::from_hex(0x888888);
    pub const TASKBAR:     Color = Color::from_hex(0x0A0A14);
    pub const TASKBAR_BTN: Color = Color::from_hex(0x1A1A30);
    pub const TASKBAR_ACT: Color = Color::from_hex(0x2A2A50);
    pub const CLOSE_BTN:   Color = Color::from_hex(0x8B1A1A);
    pub const CURSOR:      Color = Color::from_hex(0xFFFFFF);
    pub const BORDER:      Color = Color::from_hex(0x333355);
    pub const BORDER_ACT:  Color = Color::from_hex(0x6C8EFF);
}

pub const TITLE_H:   usize = 22;
pub const TASKBAR_H: usize = 32;
pub const BORDER_W:  usize = 1;

// ── Window ───────────────────────────────────────────────────────────────────
pub struct Window {
    pub id:        usize,
    pub title:     String,
    pub x:         i32,
    pub y:         i32,
    pub w:         usize,
    pub h:         usize,
    pub minimized: bool,
    // flat RGBA back-buffer for the content area
    pub content:   Vec<u32>,
    // drag state
    drag_off_x: i32,
    drag_off_y: i32,
    pub dragging:  bool,
}

impl Window {
    pub fn new(id: usize, title: &str, x: i32, y: i32, w: usize, h: usize) -> Self {
        let mut content = Vec::new();
        content.resize(w * h, 0x141414); // default background
        Window {
            id, title: String::from(title),
            x, y, w, h, minimized: false,
            content,
            drag_off_x: 0, drag_off_y: 0, dragging: false,
        }
    }

    /// Total bounding rect including title bar and border
    pub fn outer_x(&self) -> i32 { self.x - BORDER_W as i32 }
    pub fn outer_y(&self) -> i32 { self.y - TITLE_H as i32 - BORDER_W as i32 }
    pub fn outer_w(&self) -> usize { self.w + BORDER_W * 2 }
    pub fn outer_h(&self) -> usize { self.h + TITLE_H + BORDER_W * 2 }

    pub fn title_hit(&self, mx: i32, my: i32) -> bool {
        mx >= self.outer_x() && mx < self.outer_x() + self.outer_w() as i32
            && my >= self.outer_y() && my < self.y
    }

    pub fn close_hit(&self, mx: i32, my: i32) -> bool {
        let cx = self.outer_x() + self.outer_w() as i32 - 18;
        let cy = self.outer_y() + 4;
        mx >= cx && mx < cx + 14 && my >= cy && my < cy + 14
    }

    pub fn content_hit(&self, mx: i32, my: i32) -> bool {
        mx >= self.x && mx < self.x + self.w as i32
            && my >= self.y && my < self.y + self.h as i32
    }
}

// ── Desktop ──────────────────────────────────────────────────────────────────
pub struct Desktop {
    pub windows:  Vec<Window>,
    pub focused:  Option<usize>,   // window id
    next_id:      usize,
    pub fb_w:     usize,
    pub fb_h:     usize,
    prev_btn:     u8,
}

impl Desktop {
    pub fn new(fb_w: usize, fb_h: usize) -> Self {
        Desktop { windows: Vec::new(), focused: None, next_id: 0, fb_w, fb_h, prev_btn: 0 }
    }

    pub fn add_window(&mut self, title: &str, x: i32, y: i32, w: usize, h: usize) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.windows.push(Window::new(id, title, x, y, w, h));
        self.focused = Some(id);
        id
    }

    fn bring_to_front(&mut self, id: usize) {
        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let win = self.windows.remove(pos);
            self.windows.push(win);
            self.focused = Some(id);
        }
    }

    pub fn update_mouse(&mut self, mx: i32, my: i32, buttons: u8) {
        let clicked    = buttons & 0x01 != 0 && self.prev_btn & 0x01 == 0;
        let released   = buttons & 0x01 == 0 && self.prev_btn & 0x01 != 0;
        let held       = buttons & 0x01 != 0;
        self.prev_btn  = buttons;

        // Handle dragging
        if held {
            if let Some(fid) = self.focused {
                if let Some(win) = self.windows.iter_mut().find(|w| w.id == fid) {
                    if win.dragging {
                        win.x = mx - win.drag_off_x;
                        win.y = my - win.drag_off_y;
                        // clamp inside screen
                        win.x = win.x.max(0).min(self.fb_w as i32 - win.w as i32);
                        win.y = win.y.max(TITLE_H as i32).min(self.fb_h as i32 - TASKBAR_H as i32 - 1);
                        return;
                    }
                }
            }
        }

        if released {
            for win in &mut self.windows { win.dragging = false; }
        }

        if clicked {
            // Check windows top-to-bottom (last = topmost)
            let mut hit_id = None;
            for win in self.windows.iter().rev() {
                if win.minimized { continue; }
                if win.close_hit(mx, my) || win.title_hit(mx, my) || win.content_hit(mx, my) {
                    hit_id = Some(win.id);
                    break;
                }
            }

            if let Some(id) = hit_id {
                self.bring_to_front(id);
                let win = self.windows.iter_mut().find(|w| w.id == id).unwrap();
                if win.close_hit(mx, my) {
                    win.minimized = true;
                } else if win.title_hit(mx, my) {
                    win.dragging    = true;
                    win.drag_off_x  = mx - win.x;
                    win.drag_off_y  = my - win.y;
                }
            }
        }

        // Taskbar clicks: un-minimize
        if clicked && my > self.fb_h as i32 - TASKBAR_H as i32 {
            let slot_w = 120usize;
            let slot   = (mx as usize).min(self.fb_w) / slot_w;
            if let Some(win) = self.windows.get_mut(slot) {
                win.minimized = !win.minimized;
                let id = win.id;
                self.bring_to_front(id);
            }
        }
    }

    /// Render everything onto `display`.
    pub fn render(&self, display: &mut Display, cursor_x: i32, cursor_y: i32) {
        // Desktop background
        display.clear(pal::BG);

        // Draw windows bottom-to-top
        for win in &self.windows {
            if win.minimized { continue; }
            let focused = self.focused == Some(win.id);
            self.draw_window(display, win, focused);
        }

        // Taskbar
        self.draw_taskbar(display);

        // Cursor (4×4 cross)
        let cx = cursor_x as usize;
        let cy = cursor_y as usize;
        display.fill_rect(cx.saturating_sub(4), cy, 9, 1, pal::CURSOR);
        display.fill_rect(cx, cy.saturating_sub(4), 1, 9, pal::CURSOR);
    }

    fn draw_window(&self, display: &mut Display, win: &Window, focused: bool) {
        let ox = win.outer_x();
        let oy = win.outer_y();
        let ow = win.outer_w();
        let oh = win.outer_h();

        // Border
        let border_col = if focused { pal::BORDER_ACT } else { pal::BORDER };
        if ox >= 0 && oy >= 0 {
            display.fill_rect(ox as usize, oy as usize, ow, oh, border_col);
        }

        // Title bar
        let title_col = if focused { pal::WIN_TITLE_A } else { pal::WIN_TITLE };
        let tx = (win.x as usize).max(0);
        let ty = (win.outer_y() + BORDER_W as i32) as usize;
        display.fill_rect(tx, ty, win.w, TITLE_H, title_col);

        // Title text
        display.draw_text(tx + 6, ty + 5, &win.title, pal::TEXT, 1);

        // Close button
        let close_x = tx + win.w - 18;
        display.fill_rect(close_x, ty + 4, 14, 14, pal::CLOSE_BTN);
        display.draw_text(close_x + 4, ty + 5, "x", pal::TEXT, 1);

        // Content area
        display.fill_rect(win.x as usize, win.y as usize, win.w, win.h, pal::WIN_BG);

        // Blit window content buffer
        for row in 0..win.h {
            for col in 0..win.w {
                let px = win.content[row * win.w + col];
                let c = Color { r: ((px >> 16) & 0xFF) as u8, g: ((px >> 8) & 0xFF) as u8, b: (px & 0xFF) as u8 };
                if c.r != 0x14 || c.g != 0x14 || c.b != 0x14 { // skip default bg
                    display.put_pixel_pub(win.x as usize + col, win.y as usize + row, c);
                }
            }
        }
    }

    fn draw_taskbar(&self, display: &mut Display) {
        let ty = self.fb_h - TASKBAR_H;
        display.fill_rect(0, ty, self.fb_w, TASKBAR_H, pal::TASKBAR);
        display.fill_rect(0, ty, self.fb_w, 1, pal::ACCENT);

        // App buttons
        let slot_w = 120usize;
        for (i, win) in self.windows.iter().enumerate() {
            let bx = i * slot_w + 4;
            if bx + slot_w > self.fb_w { break; }
            let active = !win.minimized && self.focused == Some(win.id);
            let bc = if active { pal::TASKBAR_ACT } else { pal::TASKBAR_BTN };
            display.fill_rect(bx, ty + 4, slot_w - 8, TASKBAR_H - 8, bc);
            let label = if win.title.len() > 12 { &win.title[..12] } else { &win.title };
            display.draw_text(bx + 6, ty + 10, label, pal::TEXT, 1);
        }

        // Clock placeholder
        display.draw_text(self.fb_w - 80, ty + 10, "HepOS", pal::TEXT_DIM, 1);
    }
}

pub static DESKTOP: Mutex<Option<Desktop>> = Mutex::new(None);
