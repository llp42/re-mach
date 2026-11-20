// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The floating-point unit: x87, SSE and the states XSAVE adds.

use core::arch::asm;
use core::mem::MaybeUninit;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering};

use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::xcontrol::XCr0;

use super::MachineState;
use super::cpuid;
use super::memory_layout::PAGE_SIZE;
use super::per_cpu::PerCpu;
use crate::slab::RawCache;
use crate::smp::{self, MachinePerCpu as _};
use crate::sync::{CriticalSection, OnceLock};
use crate::thread::Thread;

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

/// FXSAVE's area in its 64-bit form, then the XSAVE header, as
/// `i386_xfp_save`: a whole saved state under FXSAVE, and the start of one
/// under XSAVE.
#[repr(C, align(64))]
struct SaveArea {
    control: u16,
    status: u16,
    /// The abridged tag word: a bit set for each physical register not
    /// empty.
    tag: u8,
    reserved: u8,
    opcode: u16,
    /// The last x87 instruction's address.
    instruction_pointer: u64,
    /// Its operand's address.
    data_pointer: u64,
    mxcsr: u32,
    /// The MXCSR bits the CPU supports.
    mxcsr_mask: u32,
    /// The x87 registers in stack order, 10 bytes of 16 each.
    registers: [[u8; 16]; 8],
    xmm: [[u8; 16]; 16],
    padding: [u8; 96],
    /// `XSTATE_BV`: the states the area holds.
    components: u64,
    /// `XCOMP_BV`: 0, the standard layout.
    compaction: u64,
    header_reserved: [u8; 48],
}

/// The MXCSR bits every CPU supports: the AND of their FXSAVE images'
/// masks.
static MXCSR_MASK: AtomicU32 = AtomicU32::new(u32::MAX);

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

/// The bytes `i386_XFLOAT_STATE` takes: its four-word head, then a saved
/// state.
///
/// # Panics
///
/// Before `init`.
pub(super) fn xstate_size() -> usize {
    size_of::<[i32; 4]>() + *STATE_SIZE.get().expect("fpu: not set up")
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

/// Gives `state` the current thread's x87 control word to start with, if
/// that thread has a floating-point state: from the FPU if it holds that
/// state, else from its buffer.
///
/// # Panics
///
/// If no thread runs on the CPU.
pub fn inherit(state: &MachineState) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("fpinherit: no current thread");
    let saved = thread.machine_state().fpu.load(Ordering::Relaxed);
    if saved.is_null() {
        return;
    }
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "a saved state's area is 64-byte aligned"
    )]
    let control = if ptr::eq(PerCpu::current().fpu_owner(), thread) {
        let mut control = 0_u16;
        // SAFETY: the FPU holds this thread's state, so TS is clear, and the
        // critical section keeps it there.
        unsafe {
            asm!("fnstcw ({0})", in(reg) ptr::from_mut(&mut control), options(att_syntax, nostack));
        }
        control
    } else {
        // SAFETY: the current thread's saved state, which only a switch from
        // it writes; its area, 64-byte aligned, starts with the control word,
        // and with XSAVE its header, holding XSTATE_BV, follows FXSAVE's area.
        unsafe {
            let area = saved.add(AREA_OFFSET);
            // XSAVEOPT may leave an x87 state in its initial configuration
            // unwritten, with XSTATE_BV's bit 0 clear: the default then.
            let initial = matches!(SAVE.get(), Some(Save::Xsave | Save::XsaveOptimized))
                && area.add(FXSAVE_SIZE).cast::<u64>().read() & 1 == 0;
            if initial {
                0
            } else {
                area.cast::<u16>().read()
            }
        }
    };
    state.init_control.store(control, Ordering::Relaxed);
}

/// Gives the current thread the FPU at a use that trapped, loaded with its
/// saved state; at its first use, a buffer holding the default state, and
/// the FPU loaded with that and the control word it inherited, if any.
///
/// # Panics
///
/// If the FPU holds a thread's state, or no buffer can be allocated.
pub fn no_extension_fault() {
    let thread = smp::current()
        .active_thread()
        .expect("fpnoextflt: no current thread");
    let saved = thread.machine_state().fpu.load(Ordering::Relaxed);
    let (source, control) = if saved.is_null() {
        let size = AREA_OFFSET + *STATE_SIZE.get().expect("fpu: not set up");
        let state = STATE_CACHE
            .get()
            .expect("fpu: module not set up")
            .allocate()
            .unwrap_or_else(|error| panic!("fp_load: {error:?}"));
        let default = DEFAULT_STATE.get().expect("fpu: module not set up").0;
        // SAFETY: both buffers hold `size` bytes; the new one is the
        // thread's alone.
        unsafe { state.copy_from_nonoverlapping(default, size) };
        thread
            .machine_state()
            .fpu
            .store(state.as_ptr(), Ordering::Relaxed);
        let control = thread.machine_state().init_control.load(Ordering::Relaxed);
        (default.as_ptr(), control)
    } else {
        (saved, 0)
    };
    let _section = CriticalSection::enter();
    let cpu = PerCpu::current();
    assert!(
        cpu.fpu_owner().is_null(),
        "fpnoextflt: the FPU holds a thread's state"
    );
    let features = FEATURES.get().copied().unwrap_or(0);
    // SAFETY: the FPU is this thread's from here; the source holds a state
    // a save left, past its header, 64-byte aligned.
    unsafe {
        let area = source.add(AREA_OFFSET);
        asm!("clts", options(att_syntax, nostack));
        // The mask in EDX:EAX: the states XCR0 enables.
        match SAVE.get().copied() {
            Some(Save::Fxsave) | None => {
                asm!("fxrstor64 ({0})", in(reg) area, options(att_syntax, nostack));
            }
            Some(Save::Xsave | Save::XsaveOptimized) => {
                asm!("xrstor64 ({0})", in(reg) area, in("rax") features, in("rdx") features >> 32, options(att_syntax, nostack));
            }
        }
        // A first state's inherited control word, as fpinit.
        if control != 0 {
            asm!("fldcw ({0})", in(reg) ptr::from_ref(&control), options(att_syntax, nostack));
        }
    }
    cpu.set_fpu_owner(thread);
}

/// Saves `thread`'s state to its buffer if the current CPU's FPU holds it,
/// then sets TS so the next use traps and leaves the CPU with no FPU owner;
/// inside a critical section.
pub fn save_context(thread: &Thread) {
    let cpu = PerCpu::current();
    if !ptr::eq(cpu.fpu_owner(), thread) {
        return;
    }
    let state = thread.machine_state().fpu.load(Ordering::Relaxed);
    let features = FEATURES.get().copied().unwrap_or(0);
    // SAFETY: the FPU's owner has a buffer, 64-byte aligned, and TS is
    // clear while it owns the FPU.
    unsafe {
        let area = state.add(AREA_OFFSET);
        // The mask in EDX:EAX: the states XCR0 enables.
        match SAVE.get().copied() {
            Some(Save::Fxsave) | None => {
                asm!("fxsave64 ({0})", in(reg) area, options(att_syntax, nostack));
            }
            Some(Save::Xsave) => {
                asm!("xsave64 ({0})", in(reg) area, in("rax") features, in("rdx") features >> 32, options(att_syntax, nostack));
            }
            Some(Save::XsaveOptimized) => {
                asm!("xsaveopt64 ({0})", in(reg) area, in("rax") features, in("rdx") features >> 32, options(att_syntax, nostack));
            }
        }
        Cr0::update(|flags| flags.insert(Cr0Flags::TASK_SWITCHED));
    }
    cpu.clear_fpu_owner();
}

/// Frees a thread's saved state's buffer as the thread ends; no FPU holds
/// that state, which the thread's last switch saved.
///
/// # Safety
///
/// `buffer` came from `no_extension_fault`, its thread was switched from
/// for the last time, and nothing uses it after.
///
/// # Panics
///
/// Before `module_init`.
pub unsafe fn free(buffer: NonNull<u8>) {
    let cache = STATE_CACHE.get().expect("fpu: module not set up");
    // SAFETY: guaranteed by the caller.
    unsafe { cache.free(buffer) };
}

/// `i386_float_state`: a thread's x87 state as the 387 lays it out, its
/// tag word full and its registers in stack order; its padding is fields,
/// zero as given, ignored as taken.
#[repr(C)]
pub(super) struct FloatState {
    /// The FPU's kind: `FP_387FX`, 4, with FXSAVE.
    kind: u32,
    /// Whether the state is valid.
    initialized: u32,
    control: u16,
    control_padding: u16,
    status: u16,
    status_padding: u16,
    /// The full tag word: two bits for each physical register.
    tag: u16,
    tag_padding: u16,
    /// The last x87 instruction's address.
    instruction_pointer: u32,
    /// Its code segment: 0 as given, ignored as taken.
    code_segment: u16,
    opcode: u16,
    /// Its operand's address.
    data_pointer: u32,
    /// Its data segment: 0 as given, ignored as taken.
    data_segment: u16,
    data_segment_padding: u16,
    /// The x87 registers in stack order.
    registers: [[u8; 10]; 8],
    /// The exception status: 0.
    exception_status: u32,
}

impl FloatState {
    /// Its size in words, `i386_FLOAT_STATE_COUNT`.
    pub(super) const COUNT: usize = size_of::<Self>() / size_of::<u32>();
}

/// The abridged tag word of a full one: a bit set for each register not
/// empty (3).
fn abridged_tag(tag: u16) -> u8 {
    (0..8).fold(0, |abridged, physical: u16| {
        if tag >> (2 * physical) & 3 == 3 {
            abridged
        } else {
            abridged | 1 << physical
        }
    })
}

/// The full tag word of `area`'s x87 registers, from its abridged one and
/// their values: for each physical register, 3 empty, 2 special, 1 zero or
/// 0 valid.
fn full_tag(area: &SaveArea) -> u16 {
    let top = area.status >> 11 & 7;
    (0..8).fold(0, |tag, physical: u16| {
        let class = if area.tag & 1 << physical == 0 {
            3
        } else {
            let register = &area.registers[usize::from(physical.wrapping_sub(top) & 7)];
            let significand = u64::from_le_bytes(*register.first_chunk().unwrap());
            let exponent = u16::from_le_bytes([register[8], register[9]]) & 0x7fff;
            match exponent {
                0 if significand == 0 => 1,
                // A normal number, its integer bit set.
                1..0x7fff if significand >> 63 == 1 => 0,
                _ => 2,
            }
        };
        tag | class << (2 * physical)
    })
}

/// Sets `state`'s thread's floating-point state from `float`, an
/// `i386_FLOAT_STATE`, in the buffer its next use loads: the whole area
/// cleared first, MXCSR the default, no value checked.
///
/// # Panics
///
/// For the current thread, an invalid state, a thread with no
/// floating-point state, or XSAVE: not yet.
///
/// # Safety
///
/// `state`'s thread is stopped, or is the current thread.
pub(super) unsafe fn write_state(state: &MachineState, float: &FloatState) {
    assert!(
        !smp::current()
            .active_thread()
            .is_some_and(|current| ptr::eq(current.machine_state(), state)),
        "fpu_set_state: the current thread, not yet"
    );
    assert!(
        float.initialized != 0,
        "fpu_set_state: an invalid state, not yet"
    );
    let buffer = state.fpu.load(Ordering::Relaxed);
    assert!(
        !buffer.is_null(),
        "fpu_set_state: no floating-point state, not yet"
    );
    assert!(
        matches!(SAVE.get(), Some(Save::Fxsave)),
        "fpu_set_state: XSAVE, not yet"
    );
    let area = SaveArea {
        control: float.control,
        status: float.status,
        tag: abridged_tag(float.tag),
        reserved: 0,
        opcode: float.opcode,
        // Zero-extended; the 64-bit form has no selectors.
        instruction_pointer: u64::from(float.instruction_pointer),
        data_pointer: u64::from(float.data_pointer),
        mxcsr: 0x1f80,
        mxcsr_mask: MXCSR_MASK.load(Ordering::Relaxed),
        registers: float.registers.map(|value| {
            let mut register = [0; 16];
            register[..10].copy_from_slice(&value);
            register
        }),
        xmm: [[0; 16]; 16],
        padding: [0; 96],
        // The x87 state alone.
        components: 1,
        compaction: 0,
        header_reserved: [0; 48],
    };
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "a saved state's area is 64-byte aligned"
    )]
    // SAFETY: guaranteed by the caller: the thread is stopped, so no FPU
    // holds its state and nothing reads the buffer until it runs again;
    // under FXSAVE its area is a `SaveArea`.
    unsafe {
        buffer.add(AREA_OFFSET).cast::<SaveArea>().write(area);
    }
}

/// `state`'s thread's floating-point state as `i386_FLOAT_STATE`, from the
/// buffer its last switch saved.
///
/// # Panics
///
/// For a thread with no floating-point state, or XSAVE: not yet.
///
/// # Safety
///
/// `state`'s thread is stopped.
pub(super) unsafe fn read_state(state: &MachineState) -> FloatState {
    let buffer = state.fpu.load(Ordering::Relaxed);
    assert!(
        !buffer.is_null(),
        "fpu_get_state: no floating-point state, not yet"
    );
    assert!(
        matches!(SAVE.get(), Some(Save::Fxsave)),
        "fpu_get_state: XSAVE, not yet"
    );
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "a saved state's area is 64-byte aligned"
    )]
    // SAFETY: guaranteed by the caller: the thread is stopped, so its last
    // switch saved its state there and cleared the CPU's FPU owner before
    // the stop's Release, which `wait_until_stopped` acquired; nothing
    // writes the buffer until it runs again. Under FXSAVE its area is a
    // `SaveArea`.
    let area = unsafe { &*buffer.add(AREA_OFFSET).cast::<SaveArea>() };
    FloatState {
        kind: 4,
        // Saved: fp_valid.
        initialized: 1,
        control: area.control,
        control_padding: 0,
        status: area.status,
        status_padding: 0,
        tag: full_tag(area),
        tag_padding: 0,
        // The low halves; the 64-bit form has no selectors.
        instruction_pointer: u32::try_from(area.instruction_pointer & 0xffff_ffff).unwrap(),
        code_segment: 0,
        opcode: area.opcode,
        data_pointer: u32::try_from(area.data_pointer & 0xffff_ffff).unwrap(),
        data_segment: 0,
        data_segment_padding: 0,
        registers: area
            .registers
            .map(|register| *register.first_chunk().unwrap()),
        exception_status: 0,
    }
}
