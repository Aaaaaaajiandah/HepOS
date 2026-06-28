use core::arch::asm;

pub const KERNEL_CS: u64 = 0x08;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtEntry {
    limit_low:   u16,
    base_low:    u16,
    base_mid:    u8,
    access:      u8,
    granularity: u8,
    base_high:   u8,
}

impl GdtEntry {
    const fn null() -> Self {
        Self { limit_low: 0, base_low: 0, base_mid: 0, access: 0, granularity: 0, base_high: 0 }
    }
    const fn code64() -> Self {
        Self { limit_low: 0xFFFF, base_low: 0, base_mid: 0, access: 0x9A, granularity: 0x20, base_high: 0 }
    }
    const fn data64() -> Self {
        Self { limit_low: 0xFFFF, base_low: 0, base_mid: 0, access: 0x92, granularity: 0x00, base_high: 0 }
    }
}

#[repr(C, packed)]
struct Gdtr {
    size:   u16,
    offset: u64,
}

static GDT: [GdtEntry; 3] = [
    GdtEntry::null(),
    GdtEntry::code64(),
    GdtEntry::data64(),
];

pub fn init() {
    let gdtr = Gdtr {
        size:   (core::mem::size_of::<[GdtEntry; 3]>() - 1) as u16,
        offset: GDT.as_ptr() as u64,
    };
    unsafe {
        asm!("lgdt [{}]", in(reg) &gdtr, options(nostack, readonly));
    }
}
