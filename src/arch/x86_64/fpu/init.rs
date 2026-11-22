// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::arch::asm;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::xcontrol::XCr0;

use super::save::{AREA_OFFSET, FEATURES, SAVE, STATE_SIZE, Save, SaveArea};
use super::state::{DEFAULT_STATE, DefaultState, STATE_CACHE};
use crate::arch::x86_64::cpuid;
use crate::arch::x86_64::memory_layout::PAGE_SIZE;
use crate::slab::RawCache;

/// The XCR0 states left off: MPX's two, gone from newer CPUs, and AMX's
/// two, whose tiles alone take 8 KiB.
const LEFT_OUT: u64 = 1 << 3 | 1 << 4 | 1 << 17 | 1 << 18;

/// The MXCSR bits every CPU supports: the AND of their FXSAVE images'
/// masks.
pub(super) static MXCSR_MASK: AtomicU32 = AtomicU32::new(u32::MAX);

/// Sets up the boot CPU's FPU for threads: the kernel never uses it, and a
/// thread's first use traps. Unmasked SIMD exceptions raise #UD, not #XM.
///
/// # Panics
///
/// On a second call.
pub fn init() {
    let xsave = cpuid::features().has_xsave();
    if xsave {
        // Read raw: raw-cpuid drops the state bits it doesn't know, and EDX's.
        let supported = raw_cpuid::cpuid!(0xd, 0);
        let features = (u64::from(supported.edx) << 32 | u64::from(supported.eax)) & !LEFT_OUT;
        assert!(FEATURES.set(features).is_ok(), "fpu: set up twice");
    }
    init_cpu();
    let (save, size) = if xsave {
        // Read again for the size of the states XCR0 now enables.
        let state = cpuid::cpu_id()
            .get_extended_state_info()
            .expect("fpu: no CPUID leaf 0xd");
        let size = usize::try_from(state.xsave_area_size_enabled_features()).unwrap();
        let save = if state.has_xsaveopt() {
            Save::XsaveOptimized
        } else {
            Save::Xsave
        };
        (save, size)
    } else {
        // The XSAVE header too, which a FLOAT set writes and FXSAVE keeps.
        (Save::Fxsave, size_of::<SaveArea>())
    };
    assert!(
        SAVE.set(save).is_ok() && STATE_SIZE.set(size).is_ok(),
        "fpu: set up twice"
    );
}

/// Sets up the current CPU's FPU registers as `init` chose: TS set, so a
/// thread's first use traps, and with XSAVE, XCR0 the boot CPU's states.
/// ANDs the CPU's MXCSR mask into `MXCSR_MASK`.
pub fn init_cpu() {
    // SAFETY: the kernel is built without floating point, so the FPU is
    // free here; TS is set below, before any thread runs.
    unsafe {
        Cr0::update(|flags| {
            flags.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
            flags.insert(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR);
        });
        Cr4::update(|flags| {
            flags.insert(Cr4Flags::OSFXSR);
            // Firmware may leave it set.
            flags.remove(Cr4Flags::OSXMMEXCPT_ENABLE);
        });
    }
    if let Some(&features) = FEATURES.get() {
        // SAFETY: the boot CPU has XSAVE and supports these states, x87 and
        // SSE among them, and the APs have its features; one XCR0 on every
        // CPU keeps `SAVE` and `STATE_SIZE` right.
        unsafe {
            Cr4::update(|flags| flags.insert(Cr4Flags::OSXSAVE));
            XCr0::write_raw(features);
        }
    }
    let mut image = MaybeUninit::<SaveArea>::uninit();
    // SAFETY: TS is clear, and the image is 64-byte aligned; FXSAVE writes
    // its mask.
    let mask = unsafe {
        asm!("fxsave64 ({0})", in(reg) image.as_mut_ptr(), options(att_syntax, nostack));
        (&raw const (*image.as_ptr()).mxcsr_mask).read()
    };
    // 0 from a CPU older than the field: all but DAZ.
    MXCSR_MASK.fetch_and(if mask == 0 { 0xffbf } else { mask }, Ordering::Relaxed);
    // SAFETY: as above; a thread's first use traps from here.
    unsafe { Cr0::update(|flags| flags.insert(Cr0Flags::TASK_SWITCHED)) };
}

/// Sets up threads' floating-point state, on the boot CPU: the cache of
/// saved states, sized as `init` found, and the default state, saved from a
/// freshly initialised FPU.
///
/// # Panics
///
/// Before `init`, if no buffer can be allocated, or on a second call.
pub fn module_init() {
    let size = *STATE_SIZE.get().expect("fpu: not set up");
    let cache = RawCache::new("i386_fpsave_state", AREA_OFFSET + size, 64, PAGE_SIZE);
    assert!(STATE_CACHE.set(cache).is_ok(), "fpu: module set up twice");
    let cache = STATE_CACHE.get().unwrap();
    cache.register();
    let buffer = cache
        .allocate()
        .unwrap_or_else(|error| panic!("fpu_module_init: {error:?}"));
    // SAFETY: a fresh buffer of `AREA_OFFSET + size` bytes, 64-byte
    // aligned; the FPU is the kernel's own while TS is clear here, and TS
    // is set again before any thread runs.
    unsafe {
        buffer.write_bytes(0, AREA_OFFSET + size);
        let area = buffer.add(AREA_OFFSET).as_ptr();
        asm!("clts", "fninit", options(att_syntax, nostack));
        match SAVE.get().copied() {
            Some(Save::Fxsave) | None => {
                asm!("fxsave64 ({0})", in(reg) area, options(att_syntax, nostack));
            }
            Some(Save::Xsave) => {
                asm!("xsave64 ({0})", in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(att_syntax, nostack));
            }
            Some(Save::XsaveOptimized) => {
                asm!("xsaveopt64 ({0})", in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(att_syntax, nostack));
            }
        }
        Cr0::update(|flags| flags.insert(Cr0Flags::TASK_SWITCHED));
    }
    assert!(
        DEFAULT_STATE.set(DefaultState(buffer)).is_ok(),
        "fpu: module set up twice"
    );
}
