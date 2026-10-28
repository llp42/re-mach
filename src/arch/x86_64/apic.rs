// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Local APIC; `enable` chooses its mode.

use x86_64::registers::model_specific::{ApicBase, ApicBaseFlags};

/// Turns on the boot CPU's local APIC in xAPIC mode.
pub fn enable() {
    let (base, mut flags) = ApicBase::read();
    if flags.contains(ApicBaseFlags::X2APIC_ENABLE) {
        // TODO: support x2APIC; keep it here instead of leaving it.
        flags.remove(ApicBaseFlags::LAPIC_ENABLE | ApicBaseFlags::X2APIC_ENABLE);
        // SAFETY: same base; x2APIC to disabled, the only way back to xAPIC.
        unsafe { ApicBase::write(base, flags) };
    }
    flags.insert(ApicBaseFlags::LAPIC_ENABLE | ApicBaseFlags::BSP);
    // SAFETY: same base; disabled or xAPIC to xAPIC.
    unsafe { ApicBase::write(base, flags) };
}
