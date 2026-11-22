// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering;

use super::init::MXCSR_MASK;
use super::save::{AREA_OFFSET, SAVE, STATE_SIZE, Save, SaveArea, abridged_tag, full_tag};
use crate::arch::x86_64::MachineState;
use crate::error::Error;
use crate::slab::RawCache;
use crate::smp;
use crate::sync::OnceLock;

/// The bytes `i386_XFLOAT_STATE` takes: its four-word head, then a saved
/// state.
///
/// # Panics
///
/// Before `init`.
pub(in crate::arch::x86_64) fn xstate_size() -> usize {
    size_of::<ExtendedFloatState>() + *STATE_SIZE.get().expect("fpu: not set up")
}

/// The cache saved states come from, once `module_init` sizes it.
pub(super) static STATE_CACHE: OnceLock<RawCache> = OnceLock::new();

/// A buffer holding the state a fresh FPU has, which threads start from.
pub(super) struct DefaultState(pub(super) NonNull<u8>);

// SAFETY: written once, before it is shared, and only read after.
unsafe impl Send for DefaultState {}
// SAFETY: as above.
unsafe impl Sync for DefaultState {}

/// The default state, once `module_init` saves it.
pub(super) static DEFAULT_STATE: OnceLock<DefaultState> = OnceLock::new();

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
pub(in crate::arch::x86_64) struct FloatState {
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
    pub(in crate::arch::x86_64) const COUNT: usize = size_of::<Self>() / size_of::<u32>();
}

/// `i386_xfloat_state`'s head; a saved state follows it as
/// `i386_xfp_save`, `xstate_size` bytes in all.
#[repr(C)]
struct ExtendedFloatState {
    /// The FPU's kind: `FP_387FX`, 4, with FXSAVE.
    kind: u32,
    /// Whether the state is valid.
    initialized: u32,
    /// The exception status: 0.
    exception_status: u32,
    /// How the state that follows was saved: `Save::number`.
    save_kind: u32,
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
pub(in crate::arch::x86_64) unsafe fn write_state(state: &MachineState, float: &FloatState) {
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
pub(in crate::arch::x86_64) unsafe fn read_state(state: &MachineState) -> FloatState {
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

/// Writes `state`'s thread's floating-point state into `words` as
/// `i386_XFLOAT_STATE`, from the buffer its last switch saved: the head,
/// then the saved state field by field, its padding 0.
///
/// # Panics
///
/// For a thread with no floating-point state, or XSAVE: not yet.
///
/// # Safety
///
/// `state`'s thread is stopped, and `words` holds `xstate_size` bytes.
pub(in crate::arch::x86_64) unsafe fn read_extended_state(state: &MachineState, words: &mut [u32]) {
    let buffer = state.fpu.load(Ordering::Relaxed);
    assert!(
        !buffer.is_null(),
        "fpu_get_state: no floating-point state, not yet"
    );
    let save = *SAVE.get().expect("fpu: not set up");
    assert!(
        matches!(save, Save::Fxsave),
        "fpu_get_state: XSAVE, not yet"
    );
    let head = ExtendedFloatState {
        kind: 4,
        // Saved: fp_valid.
        initialized: 1,
        exception_status: 0,
        save_kind: save.number(),
    };
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "a saved state's area is 64-byte aligned"
    )]
    // SAFETY: as in `read_state`.
    let area = unsafe { &*buffer.add(AREA_OFFSET).cast::<SaveArea>() };
    let saved = SaveArea {
        control: area.control,
        status: area.status,
        // The abridged tag word, raw.
        tag: area.tag,
        reserved: area.reserved,
        opcode: area.opcode,
        // The low halves; the 64-bit form has no selectors.
        instruction_pointer: area.instruction_pointer & 0xffff_ffff,
        data_pointer: area.data_pointer & 0xffff_ffff,
        mxcsr: area.mxcsr,
        mxcsr_mask: area.mxcsr_mask,
        registers: area.registers,
        xmm: area.xmm,
        padding: [0; 96],
        components: area.components,
        compaction: area.compaction,
        header_reserved: area.header_reserved,
    };
    let size = *STATE_SIZE.get().expect("fpu: not set up");
    // SAFETY: guaranteed by the caller: `words` holds the head, a
    // `SaveArea`, then the rest of a saved state, as the buffer holds that
    // rest past its area.
    unsafe {
        let start = words.as_mut_ptr().cast::<u8>();
        start.cast::<ExtendedFloatState>().write_unaligned(head);
        let user = start.add(size_of::<ExtendedFloatState>());
        user.cast::<SaveArea>().write_unaligned(saved);
        // The states past the header: none under FXSAVE.
        user.add(size_of::<SaveArea>()).copy_from_nonoverlapping(
            buffer.add(AREA_OFFSET + size_of::<SaveArea>()),
            size - size_of::<SaveArea>(),
        );
    }
}

/// Sets `state`'s thread's floating-point state from `words`, an
/// `i386_XFLOAT_STATE`, in the buffer its next use loads: the whole area
/// rewritten field by field, MXCSR and its mask cut to `MXCSR_MASK`, its
/// padding 0.
///
/// # Errors
///
/// `InvalidArgument` for a valid state saved otherwise than the kernel
/// saves.
///
/// # Panics
///
/// For the current thread, an invalid state, a thread with no
/// floating-point state, or XSAVE: not yet.
///
/// # Safety
///
/// `state`'s thread is stopped, or is the current thread; `words` holds
/// `xstate_size` bytes.
pub(in crate::arch::x86_64) unsafe fn write_extended_state(
    state: &MachineState,
    words: &[u32],
) -> Result<(), Error> {
    let start = words.as_ptr().cast::<u8>();
    // SAFETY: guaranteed by the caller: `words` starts with a head, and any
    // bytes make one.
    let head = unsafe { start.cast::<ExtendedFloatState>().read_unaligned() };
    let save = *SAVE.get().expect("fpu: not set up");
    if head.initialized != 0 && head.save_kind != save.number() {
        return Err(Error::InvalidArgument);
    }
    assert!(
        !smp::current()
            .active_thread()
            .is_some_and(|current| ptr::eq(current.machine_state(), state)),
        "fpu_set_state: the current thread, not yet"
    );
    assert!(
        head.initialized != 0,
        "fpu_set_state: an invalid state, not yet"
    );
    let buffer = state.fpu.load(Ordering::Relaxed);
    assert!(
        !buffer.is_null(),
        "fpu_set_state: no floating-point state, not yet"
    );
    assert!(
        matches!(save, Save::Fxsave),
        "fpu_set_state: XSAVE, not yet"
    );
    // SAFETY: guaranteed by the caller: a `SaveArea` follows the head, and
    // any bytes make one.
    let user = unsafe {
        start
            .add(size_of::<ExtendedFloatState>())
            .cast::<SaveArea>()
            .read_unaligned()
    };
    let mask = MXCSR_MASK.load(Ordering::Relaxed);
    let area = SaveArea {
        control: user.control,
        status: user.status,
        // The abridged tag word, raw.
        tag: user.tag,
        reserved: user.reserved,
        opcode: user.opcode,
        // Zero-extended; the 64-bit form has no selectors.
        instruction_pointer: user.instruction_pointer & 0xffff_ffff,
        data_pointer: user.data_pointer & 0xffff_ffff,
        mxcsr: user.mxcsr & mask,
        mxcsr_mask: user.mxcsr_mask & mask,
        registers: user.registers,
        xmm: user.xmm,
        padding: [0; 96],
        components: user.components,
        compaction: user.compaction,
        header_reserved: user.header_reserved,
    };
    let size = *STATE_SIZE.get().expect("fpu: not set up");
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "a saved state's area is 64-byte aligned"
    )]
    // SAFETY: guaranteed by the caller: the thread is stopped, so no FPU
    // holds its state and nothing reads the buffer until it runs again;
    // under FXSAVE its area is a `SaveArea`, and `words` holds the rest of
    // a saved state past the one given.
    unsafe {
        let destination = buffer.add(AREA_OFFSET);
        destination.cast::<SaveArea>().write(area);
        // The states past the header: none under FXSAVE.
        destination
            .add(size_of::<SaveArea>())
            .copy_from_nonoverlapping(
                start.add(size_of::<ExtendedFloatState>() + size_of::<SaveArea>()),
                size - size_of::<SaveArea>(),
            );
    }
    Ok(())
}
