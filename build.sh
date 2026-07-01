#!/usr/bin/env bash
set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Ensure cargo is on PATH
export PATH="$HOME/.cargo/bin:$PATH"

# ── 1. Build kernel ───────────────────────────────────────────────────────────
cd "$SCRIPT_DIR/kernel"
cargo +nightly build --release
cd "$SCRIPT_DIR"

KERNEL_ELF="$SCRIPT_DIR/kernel/target/x86_64-unknown-none/release/hepos-kernel"
if [ ! -f "$KERNEL_ELF" ]; then
    echo "ERROR: kernel build failed" >&2; exit 1
fi

# ── 2. Build limine installer (compiled from limine.c in the repo) ────────────
LIMINE_DIR="$SCRIPT_DIR/limine"
if [ ! -f "$LIMINE_DIR/limine" ]; then
    echo "Building limine installer..."
    make -C "$LIMINE_DIR"
fi

# ── 3. Build ISO image ────────────────────────────────────────────────────────
ISO_ROOT="$SCRIPT_DIR/iso_root"
ISO="$SCRIPT_DIR/hepos.iso"

rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine"
mkdir -p "$ISO_ROOT/EFI/BOOT"

cp "$KERNEL_ELF"                              "$ISO_ROOT/boot/hepos-kernel"
cp "$SCRIPT_DIR/bootloader/limine.conf"       "$ISO_ROOT/boot/limine/limine.conf"
cp "$LIMINE_DIR/limine-bios.sys"              "$ISO_ROOT/boot/limine/"
cp "$LIMINE_DIR/limine-bios-cd.bin"           "$ISO_ROOT/boot/limine/"
cp "$LIMINE_DIR/limine-uefi-cd.bin"           "$ISO_ROOT/boot/limine/"
cp "$LIMINE_DIR/BOOTX64.EFI"                  "$ISO_ROOT/EFI/BOOT/"

xorriso -as mkisofs \
    -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot -boot-load-size 4 -boot-info-table \
    --efi-boot boot/limine/limine-uefi-cd.bin \
    -efi-boot-part --efi-boot-image --protective-msdos-label \
    "$ISO_ROOT" -o "$ISO"

"$LIMINE_DIR/limine" bios-install "$ISO"
echo "ISO built: $ISO"

# ── 4. Create NVMe disk image if needed ──────────────────────────────────────
DISK="$SCRIPT_DIR/hepos_disk.img"
if [ ! -f "$DISK" ]; then
    echo "Creating 512 MB NVMe disk..."
    qemu-img create -f raw "$DISK" 512M
fi

# ── 5. Run in QEMU ───────────────────────────────────────────────────────────
qemu-system-x86_64 \
    -M q35 \
    -cpu qemu64,+x2apic \
    -m 256M \
    -cdrom "$ISO" \
    -boot d \
    -drive file="$DISK",if=none,id=nvme0,format=raw \
    -device nvme,serial=heposv1,drive=nvme0 \
    -netdev user,id=net0 \
    -device rtl8139,netdev=net0 \
    -device qemu-xhci,id=xhci \
    -device usb-tablet,bus=xhci.0 \
    -vga std \
    -display sdl \
    -serial stdio \
    -no-reboot \
    -no-shutdown
