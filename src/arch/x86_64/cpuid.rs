// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! CPU identification.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

use raw_cpuid::{CpuId, FeatureInfo};

struct Features(UnsafeCell<MaybeUninit<FeatureInfo>>);

// SAFETY: written once by `discover`, before any reader.
unsafe impl Sync for Features {}

static FEATURES: Features = Features(UnsafeCell::new(MaybeUninit::uninit()));

/// Saves the boot CPU's CPUID leaf 1.
///
/// # Safety
///
/// Runs once, on the boot CPU, before any `features` call.
pub unsafe fn discover() {
    let info = CpuId::new().get_feature_info().expect("CPUID leaf 1");
    // SAFETY: no reader yet.
    unsafe { (*FEATURES.0.get()).write(info) };
}

/// The boot CPU's CPUID leaf 1.
pub fn features() -> &'static FeatureInfo {
    // SAFETY: `discover` ran first.
    unsafe { (*FEATURES.0.get()).assume_init_ref() }
}
