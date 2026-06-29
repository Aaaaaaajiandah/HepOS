use core::sync::atomic::{AtomicU64, Ordering};

static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

pub fn init(hhdm: u64) {
    HHDM_OFFSET.store(hhdm, Ordering::Relaxed);
}

#[inline]
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    (HHDM_OFFSET.load(Ordering::Relaxed) + phys) as *mut u8
}

#[inline]
pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Relaxed)
}
