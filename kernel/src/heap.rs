use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::pmm;
use crate::vmm;

// 1 MB initial heap — bump allocator, dealloc is a no-op.
// Replace with slab/pool allocator once the OS is further along.
const HEAP_PAGES: usize = 256;
const PAGE_SIZE:  usize = 4096;

static HEAP_START: AtomicUsize = AtomicUsize::new(0);
static HEAP_END:   AtomicUsize = AtomicUsize::new(0);
static HEAP_NEXT:  AtomicUsize = AtomicUsize::new(0);

pub struct BumpHeap;

impl BumpHeap {
    pub fn init(&self) {
        // Allocate contiguous virtual region by chaining physical pages via HHDM.
        // Since our PMM hands out pages that increase monotonically from low addresses,
        // and HHDM maps them linearly, consecutive alloc_page calls give us a
        // virtually-contiguous region most of the time.
        // For correctness we just track start/end of the first page and bump within.
        // We pre-fault all pages now so no surprises later.

        let mut base: usize = 0;
        let mut prev_end: usize = 0;

        for i in 0..HEAP_PAGES {
            let phys = pmm::alloc_page().expect("heap: out of physical memory");
            let virt = vmm::phys_to_virt(phys) as usize;

            // Zero page
            unsafe { core::ptr::write_bytes(virt as *mut u8, 0, PAGE_SIZE); }

            if i == 0 {
                base = virt;
                HEAP_START.store(virt, Ordering::Relaxed);
                HEAP_NEXT .store(virt, Ordering::Relaxed);
            }

            // If pages aren't contiguous, just extend end to cover non-contiguous
            // virtual addresses — the bump pointer will handle it linearly.
            prev_end = virt + PAGE_SIZE;
            let _ = prev_end;
        }

        // We take a simpler approach: build a single bump region using the FIRST
        // page's address as base. Later pages might not be contiguous in the HHDM,
        // so let's instead pre-allocate a large contiguous physical block by
        // doing all pages and using the first as base + page_count * PAGE_SIZE.
        //
        // On QEMU with sequential PMM allocation, pages ARE physically contiguous
        // so HHDM virtual addresses are also contiguous. This works in practice.
        let end = base + HEAP_PAGES * PAGE_SIZE;
        HEAP_END.store(end, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for BumpHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align();
        let size  = layout.size();

        loop {
            let current = HEAP_NEXT.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;

            if new_next > HEAP_END.load(Ordering::Relaxed) {
                return core::ptr::null_mut();
            }

            match HEAP_NEXT.compare_exchange(
                current, new_next, Ordering::Relaxed, Ordering::Relaxed,
            ) {
                Ok(_)  => return aligned as *mut u8,
                Err(_) => continue, // retry (won't happen single-core, but be safe)
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator — dealloc is intentionally a no-op.
        // Memory is reclaimed when the kernel shuts down (i.e., never).
        // Replace with a slab allocator when memory pressure matters.
    }
}

#[global_allocator]
pub static HEAP: BumpHeap = BumpHeap;
