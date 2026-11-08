// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The floating-point unit: x87, SSE and the states XSAVE adds.

use core::arch::asm;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::xcontrol::XCr0;

use super::cpuid;
use super::memory_layout::PAGE_SIZE;
use super::per_cpu::PerCpu;
use crate::slab::RawCache;
use crate::smp::{self, MachinePerCpu as _};
use crate::sync::{CriticalSection, OnceLock};

/// How a thread's floating-point state is saved; always in the standard
/// layout, so the state a thread reads back is laid out alike on every CPU.
#[derive(Clone, Copy)]
enum Save {
    /// FXSAVE: x87 and SSE.
    Fxsave,
    /// XSAVE: every state XCR0 enables.
    Xsave,
    /// XSAVEOPT: as XSAVE, skipping the states unchanged since the last
    /// restore.
    XsaveOptimized,
}

/// The save instruction, once `init` picks it.
static SAVE: OnceLock<Save> = OnceLock::new();

/// The states XCR0 enables, when the CPU has XSAVE.
static FEATURES: OnceLock<u64> = OnceLock::new();

/// The bytes a thread's saved state takes.
static STATE_SIZE: OnceLock<usize> = OnceLock::new();

/// The XCR0 states left off: MPX's two, gone from newer CPUs, and AMX's
/// two, whose tiles alone take 8 KiB.
const LEFT_OUT: u64 = 1 << 3 | 1 << 4 | 1 << 17 | 1 << 18;

/// The size of FXSAVE's area.
const FXSAVE_SIZE: usize = 512;

/// Sets up the boot CPU's FPU for threads: the kernel never uses it, and a
/// thread's first use traps. Unmasked SIMD exceptions raise #UD, not #XM.
///
/// # Panics
///
/// On a second call.
pub fn init() {
    // SAFETY: the kernel is built without floating point, so setting TS
    // traps only threads' instructions.
    unsafe {
        Cr0::update(|flags| {
            flags.remove(Cr0Flags::EMULATE_COPROCESSOR);
            flags.insert(
                Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR | Cr0Flags::TASK_SWITCHED,
            );
        });
        Cr4::update(|flags| {
            flags.insert(Cr4Flags::OSFXSR);
            // Firmware may leave it set.
            flags.remove(Cr4Flags::OSXMMEXCPT_ENABLE);
        });
    }
    let (save, size) = if cpuid::features().has_xsave() {
        let supported = raw_cpuid::cpuid!(0xd, 0);
        let features = (u64::from(supported.edx) << 32 | u64::from(supported.eax)) & !LEFT_OUT;
        // SAFETY: the CPU has XSAVE, and these are states it supports, x87
        // and SSE among them.
        unsafe {
            Cr4::update(|flags| flags.insert(Cr4Flags::OSXSAVE));
            XCr0::write_raw(features);
        }
        assert!(FEATURES.set(features).is_ok(), "fpu: set up twice");
        // The size of the states XCR0 now enables.
        let size = usize::try_from(raw_cpuid::cpuid!(0xd, 0).ebx).unwrap();
        let save = if raw_cpuid::cpuid!(0xd, 1).eax & 1 != 0 {
            Save::XsaveOptimized
        } else {
            Save::Xsave
        };
        (save, size)
    } else {
        (Save::Fxsave, FXSAVE_SIZE)
    };
    assert!(
        SAVE.set(save).is_ok() && STATE_SIZE.set(size).is_ok(),
        "fpu: set up twice"
    );
}

/// Where a saved state's area starts in its buffer, past its header; the
/// areas XSAVE writes are 64-byte aligned.
const AREA_OFFSET: usize = 64;

/// The cache saved states come from, once `module_init` sizes it.
static STATE_CACHE: OnceLock<RawCache> = OnceLock::new();

/// A buffer holding the state a fresh FPU has, which threads start from.
struct DefaultState(NonNull<u8>);

// SAFETY: written once, before it is shared, and only read after.
unsafe impl Send for DefaultState {}
// SAFETY: as above.
unsafe impl Sync for DefaultState {}

/// The default state, once `module_init` saves it.
static DEFAULT_STATE: OnceLock<DefaultState> = OnceLock::new();

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

/// Gives the current thread the FPU at its first use, which trapped: a
/// buffer for its state, holding the default one, and the FPU loaded with it.
///
/// # Panics
///
/// If the FPU holds a thread's state, or the thread has a buffer: not yet;
/// if no buffer can be allocated.
pub fn no_extension_fault() {
    let thread = smp::current()
        .active_thread()
        .expect("fpnoextflt: no current thread");
    assert!(
        thread.pcb().fpu.load(Ordering::Relaxed).is_null(),
        "fp_load: a saved state, not yet"
    );
    let size = AREA_OFFSET + *STATE_SIZE.get().expect("fpu: not set up");
    let state = STATE_CACHE
        .get()
        .expect("fpu: module not set up")
        .allocate()
        .unwrap_or_else(|error| panic!("fp_load: {error:?}"));
    let default = DEFAULT_STATE.get().expect("fpu: module not set up").0;
    // SAFETY: both buffers hold `size` bytes; the new one is the thread's
    // alone.
    unsafe { state.copy_from_nonoverlapping(default, size) };
    thread.pcb().fpu.store(state.as_ptr(), Ordering::Relaxed);
    let _section = CriticalSection::enter();
    let cpu = PerCpu::current();
    assert!(
        cpu.fpu_owner().is_null(),
        "fpnoextflt: the FPU holds a thread's state, not yet"
    );
    // SAFETY: the FPU is this thread's from here; the default area holds a
    // state `module_init` saved, 64-byte aligned.
    unsafe {
        let area = default.add(AREA_OFFSET).as_ptr();
        asm!("clts", options(att_syntax, nostack));
        match SAVE.get().copied() {
            Some(Save::Fxsave) | None => {
                asm!("fxrstor64 ({0})", in(reg) area, options(att_syntax, nostack));
            }
            Some(Save::Xsave | Save::XsaveOptimized) => {
                asm!("xrstor64 ({0})", in(reg) area, in("eax") u32::MAX, in("edx") u32::MAX, options(att_syntax, nostack));
            }
        }
    }
    cpu.set_fpu_owner(thread);
}
