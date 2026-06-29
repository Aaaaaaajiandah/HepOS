use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use spin::Mutex;

use crate::pmm;
use crate::vmm;

const HEAP_PAGES: usize = 256; // 1 MB initial heap
const PAGE_SIZE:  usize = 4096;

// Each free block: [size: usize | next: *mut FreeBlock | ...free bytes...]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

struct HeapInner {
    head: *mut FreeBlock,
}

unsafe impl Send for HeapInner {}

pub struct Heap(Mutex<HeapInner>);

impl Heap {
    pub const fn new() -> Self {
        Heap(Mutex::new(HeapInner { head: ptr::null_mut() }))
    }

    pub fn init(&self) {
        let mut inner = self.0.lock();

        for i in 0..HEAP_PAGES {
            let phys = pmm::alloc_page().expect("heap: out of physical memory");
            let virt = vmm::phys_to_virt(phys) as *mut FreeBlock;

            // Zero the page
            unsafe { ptr::write_bytes(virt as *mut u8, 0, PAGE_SIZE); }

            let block = unsafe { &mut *virt };
            block.size = PAGE_SIZE - core::mem::size_of::<FreeBlock>();
            block.next = inner.head;
            inner.head = virt;

            let _ = i;
        }
    }

    unsafe fn alloc_inner(&self, layout: Layout) -> *mut u8 {
        let size   = layout.size().max(core::mem::size_of::<FreeBlock>());
        let align  = layout.align();
        let header = core::mem::size_of::<FreeBlock>();

        let mut inner = self.0.lock();
        let mut current = &mut inner.head as *mut *mut FreeBlock;

        while !(*current).is_null() {
            let block = &mut **current;
            let raw   = (*current as *mut u8).add(header);

            // Find aligned start within this block
            let align_off = raw.align_offset(align);
            let needed    = align_off + size;

            if block.size >= needed {
                let ptr = raw.add(align_off);

                // Split if leftover is large enough for another block
                let leftover = block.size - needed;
                if leftover >= header + 8 {
                    let split = ptr.add(size) as *mut FreeBlock;
                    (*split).size = leftover - header;
                    (*split).next = block.next;
                    *current = split;
                } else {
                    *current = block.next;
                }

                return ptr;
            }

            current = &mut (**current).next as *mut *mut FreeBlock;
        }

        ptr::null_mut()
    }

    unsafe fn dealloc_inner(&self, ptr: *mut u8, layout: Layout) {
        let size   = layout.size().max(core::mem::size_of::<FreeBlock>());
        let header = core::mem::size_of::<FreeBlock>();

        // Walk back to block header (may be before ptr due to alignment)
        // We store the block at ptr itself for simplicity (align is already handled at alloc)
        let block = ptr as *mut FreeBlock;
        (*block).size = size;

        let mut inner = self.0.lock();
        (*block).next = inner.head;
        inner.head = block;
    }
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.alloc_inner(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.dealloc_inner(ptr, layout)
    }
}

#[global_allocator]
pub static HEAP: Heap = Heap::new();
