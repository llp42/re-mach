// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Terminals; so far only their pseudo-DMA input settings.

use crate::config::HZ;
use crate::sync::OnceLock;

/// How many line speeds Mach's terminal interface numbers, `B0` to
/// `B115200`.
const SPEEDS: usize = 18;

/// Pseudo-DMA input settings, by line speed.
#[expect(dead_code, reason = "the terminal input code reads them")]
struct PseudoDma {
    /// How long to wait for more input, in ticks (`pdma_timeouts`).
    timeouts: [u64; SPEEDS],
    /// How many characters to gather before delivering them
    /// (`pdma_water_mark`).
    water_marks: [usize; SPEEDS],
}

/// The pseudo-DMA input settings; once `init` computes them.
static PSEUDO_DMA: OnceLock<PseudoDma> = OnceLock::new();

/// Computes the pseudo-DMA input settings for `HZ`, none below 300 baud,
/// on the boot CPU.
///
/// # Panics
///
/// On a second call.
pub fn init() {
    let mut timeouts = [0; SPEEDS];
    // From B300 (7): a character's time at these rates, in characters a
    // second, plus 2 ticks.
    for (timeout, rate) in timeouts[7..]
        .iter_mut()
        .zip([30, 60, 120, 180, 240, 480, 960, 1440, 1920, 5760, 11520])
    {
        *timeout = u64::from(HZ) / rate + 2;
    }
    let mut water_marks = [0; SPEEDS];
    // To B4800, a fifth of these rates; from B9600 (13), half the 4096-byte
    // input queue.
    for (mark, rate) in water_marks[7..13]
        .iter_mut()
        .zip([120, 120, 120, 180, 240, 480])
    {
        *mark = rate / 5;
    }
    water_marks[13..].fill(4096 / 2);
    assert!(
        PSEUDO_DMA
            .set(PseudoDma {
                timeouts,
                water_marks
            })
            .is_ok(),
        "chario_init: set up twice"
    );
}
