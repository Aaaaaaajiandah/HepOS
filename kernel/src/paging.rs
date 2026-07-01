use crate::{pmm, vmm};

pub const PRESENT: u64 = 1 << 0;
pub const WRITE:   u64 = 1 << 1;
pub const USER:    u64 = 1 << 2;
pub const NOCACHE: u64 = 1 << 4;

fn cr3_phys() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) v, options(nomem, nostack)); }
    v & !0xFFF
}

unsafe fn invlpg(virt: u64) {
    core::arch::asm!("invlpg [{0}]", in(reg) virt, options(nomem, nostack));
}

unsafe fn get_or_make(table_phys: u64, idx: usize) -> u64 {
    let ptr = (vmm::phys_to_virt(table_phys) as *mut u64).add(idx);
    let e = ptr.read_volatile();
    if e & PRESENT != 0 {
        e & 0x000F_FFFF_FFFF_F000
    } else {
        let p = pmm::alloc_page().expect("paging: OOM");
        core::ptr::write_bytes(vmm::phys_to_virt(p), 0, 4096);
        ptr.write_volatile(p | PRESENT | WRITE);
        p
    }
}

pub fn map_page(virt: u64, phys: u64, flags: u64) {
    let i4 = ((virt >> 39) & 0x1FF) as usize;
    let i3 = ((virt >> 30) & 0x1FF) as usize;
    let i2 = ((virt >> 21) & 0x1FF) as usize;
    let i1 = ((virt >> 12) & 0x1FF) as usize;
    unsafe {
        let p3 = get_or_make(cr3_phys(), i4);
        let p2 = get_or_make(p3, i3);
        let p1 = get_or_make(p2, i2);
        let pt = (vmm::phys_to_virt(p1) as *mut u64).add(i1);
        pt.write_volatile(phys | flags | PRESENT | WRITE);
        invlpg(virt);
    }
}

/// Like get_or_make but sets the USER bit on intermediate page-table entries,
/// allowing user-mode traversal down to the leaf.
unsafe fn get_or_make_user(table_phys: u64, idx: usize) -> u64 {
    let ptr = (vmm::phys_to_virt(table_phys) as *mut u64).add(idx);
    let e = ptr.read_volatile();
    if e & PRESENT != 0 {
        e & 0x000F_FFFF_FFFF_F000
    } else {
        let p = pmm::alloc_page().expect("paging: OOM");
        core::ptr::write_bytes(vmm::phys_to_virt(p), 0, 4096);
        ptr.write_volatile(p | PRESENT | WRITE | USER);
        p
    }
}

/// Map a physical page into an arbitrary PML4 (not the current CR3).
/// All intermediate page-table entries get the USER bit so ring-3 code can walk them.
pub fn map_page_into(pml4_phys: u64, virt: u64, phys: u64, flags: u64) {
    let i4 = ((virt >> 39) & 0x1FF) as usize;
    let i3 = ((virt >> 30) & 0x1FF) as usize;
    let i2 = ((virt >> 21) & 0x1FF) as usize;
    let i1 = ((virt >> 12) & 0x1FF) as usize;
    unsafe {
        let p3 = get_or_make_user(pml4_phys, i4);
        let p2 = get_or_make_user(p3, i3);
        let p1 = get_or_make_user(p2, i2);
        let pt = (vmm::phys_to_virt(p1) as *mut u64).add(i1);
        pt.write_volatile(phys | flags | PRESENT);
        // No invlpg — this PML4 is not yet loaded into CR3
    }
}

/// Map a physical MMIO region at hhdm_offset + phys (uncached).
/// Safe to call even if Limine didn't include the region in its map.
pub fn map_mmio(phys: u64, size: usize) -> *mut u8 {
    let hhdm  = vmm::hhdm_offset();
    let pages = (size + 4095) / 4096;
    for i in 0..pages as u64 {
        map_page(hhdm + phys + i * 4096, phys + i * 4096, NOCACHE);
    }
    (hhdm + phys) as *mut u8
}
