// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! CPU identification.

use raw_cpuid::{CpuId, CpuIdReaderNative, FeatureInfo};

use crate::sync::OnceLock;

/// The CPUID reader, once discovered: the vendor and highest leaves,
/// which every CPU shares.
static CPU_ID: OnceLock<CpuId<CpuIdReaderNative>> = OnceLock::new();

/// The boot CPU's CPUID leaf 1, once discovered.
static FEATURES: OnceLock<FeatureInfo> = OnceLock::new();

/// Saves the CPUID reader and the boot CPU's leaf 1.
///
/// # Panics
///
/// If the CPU has no leaf 1, or on a second call.
pub fn discover() {
    let cpu_id = CpuId::new();
    let info = cpu_id.get_feature_info().expect("CPUID leaf 1");
    assert!(
        CPU_ID.set(cpu_id).is_ok() && FEATURES.set(info).is_ok(),
        "cpuid: discovered twice"
    );
}

/// The CPUID reader; each leaf is read on the CPU asking, when asked.
///
/// # Panics
///
/// Before `discover`.
pub fn cpu_id() -> &'static CpuId<CpuIdReaderNative> {
    CPU_ID.get().expect("cpuid: not discovered yet")
}

/// The boot CPU's CPUID leaf 1.
///
/// # Panics
///
/// Before `discover`.
pub fn features() -> &'static FeatureInfo {
    FEATURES.get().expect("cpuid: not discovered yet")
}
