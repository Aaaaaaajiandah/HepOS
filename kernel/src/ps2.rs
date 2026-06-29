use core::arch::asm;
use spin::Mutex;

const DATA_PORT:   u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const CMD_PORT:    u16 = 0x64;

fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack)); }
    v
}
fn outb(port: u16, val: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack)); }
}
fn wait_write() { while inb(STATUS_PORT) & 0x02 != 0 {} }
fn wait_read()  { while inb(STATUS_PORT) & 0x01 == 0 {} }

// Circular key event buffer (scancodes decoded to ASCII)
const BUF_SIZE: usize = 64;
struct KeyBuf {
    buf:  [u8; BUF_SIZE],
    head: usize,
    tail: usize,
}
impl KeyBuf {
    const fn new() -> Self { Self { buf: [0; BUF_SIZE], head: 0, tail: 0 } }
    fn push(&mut self, c: u8) {
        let next = (self.tail + 1) % BUF_SIZE;
        if next != self.head { self.buf[self.tail] = c; self.tail = next; }
    }
    pub fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail { return None; }
        let c = self.buf[self.head];
        self.head = (self.head + 1) % BUF_SIZE;
        Some(c)
    }
}

static KEYBUF: Mutex<KeyBuf> = Mutex::new(KeyBuf::new());

// US QWERTY scancode set 1 → ASCII (make codes only, shift ignored for now)
static SCANCODE_MAP: [u8; 58] = [
    0,   0x1B, b'1', b'2', b'3', b'4', b'5', b'6',  // 0x00-0x07 (0x01=ESC→\x1b)
    b'7', b'8', b'9', b'0', b'-', b'=', b'\x08', b'\t', // 0x08-0x0F
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', // 0x10-0x17
    b'o', b'p', b'[', b']', b'\n', 0,   b'a', b's', // 0x18-0x1F
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', // 0x20-0x27
    b'\'', b'`', 0,   b'\\', b'z', b'x', b'c', b'v', // 0x28-0x2F
    b'b', b'n', b'm', b',', b'.', b'/', 0,   b'*',  // 0x30-0x37
    0,   b' ', // 0x38-0x39
];

// Extended scancode state (0xE0 prefix)
static EXTENDED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// Special key codes (stored as u8 > 127, read as char via `b as char`)
pub const KEY_UP:    u8 = 0x80;
pub const KEY_DOWN:  u8 = 0x81;
pub const KEY_LEFT:  u8 = 0x82;
pub const KEY_RIGHT: u8 = 0x83;
pub const KEY_DEL:   u8 = 0x84;
pub const KEY_HOME:  u8 = 0x85;
pub const KEY_END:   u8 = 0x86;

/// Called from keyboard interrupt handler (or polled).
pub fn handle_scancode(sc: u8) {
    if sc == 0xE0 {
        EXTENDED.store(true, core::sync::atomic::Ordering::Relaxed);
        return;
    }
    let ext = EXTENDED.swap(false, core::sync::atomic::Ordering::Relaxed);

    if sc & 0x80 != 0 { return; } // key release

    let ch: u8 = if ext {
        match sc {
            0x48 => KEY_UP,
            0x50 => KEY_DOWN,
            0x4B => KEY_LEFT,
            0x4D => KEY_RIGHT,
            0x53 => KEY_DEL,
            0x47 => KEY_HOME,
            0x4F => KEY_END,
            _ => 0,
        }
    } else {
        let sc = sc as usize;
        if sc < SCANCODE_MAP.len() { SCANCODE_MAP[sc] } else { 0 }
    };

    if ch != 0 { KEYBUF.lock().push(ch); }
}

/// Non-blocking read — returns Some(char) if a key is available.
pub fn read_char() -> Option<char> {
    KEYBUF.lock().pop().map(|b| b as char)
}

/// Blocking read — spins until a key is available.
pub fn read_char_blocking() -> char {
    loop {
        if let Some(c) = read_char() { return c; }
        core::hint::spin_loop();
    }
}

pub fn init() {
    // Flush output buffer
    while inb(STATUS_PORT) & 0x01 != 0 { inb(DATA_PORT); }

    // Disable both PS/2 ports
    wait_write(); outb(CMD_PORT, 0xAD);
    wait_write(); outb(CMD_PORT, 0xA7);

    // Read and modify controller config byte
    wait_write(); outb(CMD_PORT, 0x20);
    wait_read();
    let mut config = inb(DATA_PORT);
    config &= !(1 << 0); // enable port 1 IRQ
    config &= !(1 << 6); // disable translation
    wait_write(); outb(CMD_PORT, 0x60);
    wait_write(); outb(DATA_PORT, config);

    // Enable port 1 (keyboard)
    wait_write(); outb(CMD_PORT, 0xAE);

    // Reset keyboard
    wait_write(); outb(DATA_PORT, 0xFF);
    wait_read();  inb(DATA_PORT); // ACK
    wait_read();  inb(DATA_PORT); // self-test result

    // Set scancode set 1
    wait_write(); outb(DATA_PORT, 0xF0);
    wait_read();  inb(DATA_PORT);
    wait_write(); outb(DATA_PORT, 0x01);
    wait_read();  inb(DATA_PORT);

    // Enable scanning
    wait_write(); outb(DATA_PORT, 0xF4);
    wait_read();  inb(DATA_PORT);
}

/// Poll the PS/2 data port. Only processes keyboard bytes (bit 5 = 0).
/// Mouse bytes (bit 5 = 1) are left for mouse::poll() to consume.
pub fn poll() {
    let s = inb(STATUS_PORT);
    if s & 0x01 != 0 && s & 0x20 == 0 {
        handle_scancode(inb(DATA_PORT));
    }
}
