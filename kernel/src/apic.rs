use core::arch::asm;

// x2APIC uses MSRs instead of MMIO — no virtual address mapping required.
// All xAPIC register offsets (0x000–0x3F0) map to MSR 0x800 + (offset >> 4).
const X2APIC_BASE:    u32 = 0x800;
const X2APIC_SIVR:   u32 = X2APIC_BASE + (0x0F0 >> 4); // 0x80F
const X2APIC_EOI:    u32 = X2APIC_BASE + (0x0B0 >> 4); // 0x80B
const X2APIC_LVT_TMR:u32 = X2APIC_BASE + (0x320 >> 4); // 0x832
const X2APIC_TMR_ICR:u32 = X2APIC_BASE + (0x380 >> 4); // 0x838
const X2APIC_TMR_DIV:u32 = X2APIC_BASE + (0x3E0 >> 4); // 0x83E

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const TIMER_VECTOR:        u8  = 0x20;
const SPURIOUS_VECTOR:     u8  = 0xFF;

// APIC timer: divide by 16, periodic, ~10 ms on QEMU (1 GHz bus / 16 / 625000)
const TIMER_COUNT: u32 = 625_000;

fn rdmsr(msr: u32) -> u64 {
    let lo: u32; let hi: u32;
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack)); }
    (hi as u64) << 32 | lo as u64
}

fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    unsafe { asm!("wrmsr", in("ecx") msr, in("eax") lo, in("edx") hi, options(nomem, nostack)); }
}

fn cpuid_ecx1() -> u32 {
    let ecx: u32;
    unsafe {
        asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 1u32 => _,
            out("ecx") ecx,
            out("edx") _,
            options(nomem, nostack)
        );
    }
    ecx
}

pub fn init() {
    // Verify x2APIC is supported (CPUID leaf 1, ECX bit 21)
    assert!(cpuid_ecx1() & (1 << 21) != 0, "x2APIC not supported — pass -cpu qemu64,+x2apic to QEMU");

    // Disable legacy 8259 PIC
    unsafe {
        asm!("out 0x21, al", in("al") 0xFFu8, options(nomem, nostack));
        asm!("out 0xA1, al", in("al") 0xFFu8, options(nomem, nostack));
    }

    // Enable x2APIC: set bit 10 in IA32_APIC_BASE MSR
    let base = rdmsr(IA32_APIC_BASE_MSR);
    wrmsr(IA32_APIC_BASE_MSR, base | (1 << 10));

    // Enable LAPIC via spurious interrupt vector register (bit 8 = software enable)
    wrmsr(X2APIC_SIVR, (SPURIOUS_VECTOR as u64) | (1 << 8));

    // Timer: divide by 16, periodic mode (bit 17), vector 0x20
    wrmsr(X2APIC_TMR_DIV, 0x3);
    wrmsr(X2APIC_LVT_TMR, (TIMER_VECTOR as u64) | (1 << 17));
    wrmsr(X2APIC_TMR_ICR, TIMER_COUNT as u64);
}

#[inline]
pub fn eoi() {
    wrmsr(X2APIC_EOI, 0);
}

pub fn timer_vector() -> u8 { TIMER_VECTOR }

/// Mask the APIC timer (stops preemptive scheduling).
pub fn mask_timer() {
    unsafe { wrmsr(X2APIC_LVT_TMR, rdmsr(X2APIC_LVT_TMR) | (1 << 16)); }
}

/// Unmask the APIC timer (resumes preemptive scheduling).
pub fn unmask_timer() {
    unsafe { wrmsr(X2APIC_LVT_TMR, rdmsr(X2APIC_LVT_TMR) & !(1 << 16)); }
}
