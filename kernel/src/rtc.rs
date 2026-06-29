use core::arch::asm;

fn outb(port: u16, val: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack)); }
}
fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack)); }
    v
}

fn cmos(reg: u8) -> u8 {
    outb(0x70, reg);
    inb(0x71)
}

fn bcd(v: u8) -> u8 { (v & 0x0F) + ((v >> 4) * 10) }

pub struct DateTime {
    pub hour:  u8,
    pub min:   u8,
    pub sec:   u8,
    pub day:   u8,
    pub month: u8,
    pub year:  u16,
}

pub fn now() -> DateTime {
    // Wait for RTC update-in-progress flag to clear
    while cmos(0x0A) & 0x80 != 0 {}
    let s  = bcd(cmos(0x00));
    let m  = bcd(cmos(0x02));
    let h  = bcd(cmos(0x04));
    let d  = bcd(cmos(0x07));
    let mo = bcd(cmos(0x08));
    let yr = 2000u16 + bcd(cmos(0x09)) as u16;
    DateTime { hour: h, min: m, sec: s, day: d, month: mo, year: yr }
}

/// Format as "HH:MM" into a stack buffer (no alloc).
pub fn fmt_time(buf: &mut [u8; 6]) -> &str {
    let t = now();
    buf[0] = b'0' + t.hour / 10;
    buf[1] = b'0' + t.hour % 10;
    buf[2] = b':';
    buf[3] = b'0' + t.min / 10;
    buf[4] = b'0' + t.min % 10;
    buf[5] = 0;
    core::str::from_utf8(&buf[..5]).unwrap_or("??:??")
}

/// Format as "YYYY-MM-DD" into a stack buffer.
pub fn fmt_date(buf: &mut [u8; 11]) -> &str {
    let t = now();
    let y = t.year;
    buf[0] = b'0' + (y / 1000) as u8;
    buf[1] = b'0' + ((y / 100) % 10) as u8;
    buf[2] = b'0' + ((y / 10) % 10) as u8;
    buf[3] = b'0' + (y % 10) as u8;
    buf[4] = b'-';
    buf[5] = b'0' + t.month / 10;
    buf[6] = b'0' + t.month % 10;
    buf[7] = b'-';
    buf[8] = b'0' + t.day / 10;
    buf[9] = b'0' + t.day % 10;
    buf[10] = 0;
    core::str::from_utf8(&buf[..10]).unwrap_or("????-??-??")
}
