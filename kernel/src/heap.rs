//! Slab allocator — 10 size classes (8 B … 4096 B), lazy PMM-backed free lists.
//! Large allocations (> 4096 B) go directly to/from contiguous PMM pages.
//! Dealloc is fully implemented: memory is returned to the free list or PMM.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use spin::Mutex;
use crate::{pmm, vmm};

const PAGE_SIZE: usize = 4096;

// Ten power-of-two size classes covering 8 B to one full page.
const SIZE_CLASSES: [usize; 10] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const NUM_CLASSES:  usize       = 10;

// ── Inner state ────────────────────────────────────────────────────────────────

struct SlabInner {
    /// Head of per-class free list. Each free chunk's first word is the
    /// next pointer (intrusive singly-linked list stored in the chunk itself).
    free_lists: [*mut u8; NUM_CLASSES],
}

// SAFETY: single-core kernel; all access is gated by the outer Mutex.
unsafe impl Send for SlabInner {}
unsafe impl Sync for SlabInner {}

impl SlabInner {
    const fn new() -> Self {
        Self { free_lists: [ptr::null_mut(); NUM_CLASSES] }
    }

    /// Smallest size class index that can satisfy `layout`.
    ///
    /// Each chunk of class `i` lives at an offset that is a multiple of
    /// `SIZE_CLASSES[i]` within a page-aligned PMM page, so it is naturally
    /// aligned to `SIZE_CLASSES[i]`.  We therefore require the class size
    /// to cover both the requested size and alignment.  The minimum of 8
    /// ensures every free chunk can store one pointer.
    fn class_for(layout: &Layout) -> Option<usize> {
        let min = layout.size()
            .max(layout.align())
            .max(core::mem::size_of::<usize>());
        SIZE_CLASSES.iter().position(|&sz| sz >= min)
    }

    unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        // Zero-sized types: return a well-aligned non-null dangling pointer.
        if layout.size() == 0 {
            return layout.align() as *mut u8;
        }

        // Large allocation — bypass the slab and use contiguous PMM pages.
        if layout.size() > PAGE_SIZE {
            let n = pages_for(layout.size());
            return match pmm::alloc_contiguous(n) {
                Some(phys) => vmm::phys_to_virt(phys),
                None       => ptr::null_mut(),
            };
        }

        let ci         = match Self::class_for(&layout) { Some(i) => i, None => return ptr::null_mut() };
        let chunk_size = SIZE_CLASSES[ci];

        // Fast path: pop from free list.
        if !self.free_lists[ci].is_null() {
            let node = self.free_lists[ci];
            // First word of the chunk is the next-pointer.
            self.free_lists[ci] = ptr::read(node as *const *mut u8);
            return node;
        }

        // Slow path: grab a fresh PMM page and slice it into chunks.
        let phys = match pmm::alloc_page() { Some(p) => p, None => return ptr::null_mut() };
        let base = vmm::phys_to_virt(phys);

        // Thread chunks [1 .. count) onto the free list in reverse so that
        // subsequent allocations return them in ascending address order.
        let count = PAGE_SIZE / chunk_size;
        for i in (1..count).rev() {
            let chunk = base.add(i * chunk_size);
            ptr::write(chunk as *mut *mut u8, self.free_lists[ci]);
            self.free_lists[ci] = chunk;
        }

        // Return chunk[0] directly to the caller.
        base
    }

    unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() || layout.size() == 0 {
            return;
        }

        // Large allocation — return pages to PMM.
        if layout.size() > PAGE_SIZE {
            let n    = pages_for(layout.size());
            let phys = ptr as u64 - vmm::hhdm_offset();
            for i in 0..n {
                pmm::free_page(phys + (i * PAGE_SIZE) as u64);
            }
            return;
        }

        let ci = match Self::class_for(&layout) { Some(i) => i, None => return };

        // Push the chunk onto the head of the free list.
        ptr::write(ptr as *mut *mut u8, self.free_lists[ci]);
        self.free_lists[ci] = ptr;
    }
}

#[inline]
fn pages_for(bytes: usize) -> usize {
    (bytes + PAGE_SIZE - 1) / PAGE_SIZE
}

// ── Public interface ───────────────────────────────────────────────────────────

static SLAB: Mutex<SlabInner> = Mutex::new(SlabInner::new());

pub struct SlabHeap;

impl SlabHeap {
    /// No-op: the slab is lazily initialised on first allocation.
    /// Kept so `heap::HEAP.init()` in main.rs continues to compile.
    pub fn init(&self) {}
}

unsafe impl GlobalAlloc for SlabHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        SLAB.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        SLAB.lock().dealloc(ptr, layout)
    }
}

#[global_allocator]
pub static HEAP: SlabHeap = SlabHeap;
