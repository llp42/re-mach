// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! CPU identification.

use raw_cpuid::{CpuId, FeatureInfo};

use crate::sync::OnceLock;

/// The boot CPU's CPUID leaf 1, once discovered.
static FEATURES: OnceLock<FeatureInfo> = OnceLock::new();

/// Saves the boot CPU's CPUID leaf 1.
///
/// # Panics
///
/// If the CPU has no leaf 1, or on a second call.
pub fn discover() {
    let info = CpuId::new().get_feature_info().expect("CPUID leaf 1");
    assert!(FEATURES.set(info).is_ok(), "cpuid: discovered twice");
}

/// The boot CPU's CPUID leaf 1.
///
/// # Panics
///
/// Before `discover`.
pub fn features() -> &'static FeatureInfo {
    FEATURES.get().expect("cpuid: not discovered yet")
}
