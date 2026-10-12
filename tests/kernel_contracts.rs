// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Host checks of the actual boot codec and diagnostic macros.

#[path = "../src/diagnostics.rs"]
mod diagnostics;
#[path = "../src/boot/info.rs"]
mod info;

use info::{BootInfo, Builder, Error, Framebuffer, MemoryKind, MemoryRegion};

mod arch {
    pub fn halt() -> ! {
        panic!("host halt")
    }

    pub mod serial {
        std::thread_local! {
            pub static OUTPUT: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }

        pub fn init() {
            OUTPUT.with(|output| output.borrow_mut().clear());
        }

        pub fn write(bytes: &[u8]) {
            OUTPUT.with(|output| output.borrow_mut().extend_from_slice(bytes));
        }
    }
}

fn region() -> MemoryRegion {
    MemoryRegion {
        physical_start: 0x8000,
        byte_len: 0x2000,
        kind: MemoryKind::Available,
    }
}

fn framebuffer() -> Framebuffer {
    Framebuffer {
        physical_address: 0x1234_5678,
        byte_len: 32,
        width: 3,
        height: 2,
        stride_bytes: 16,
        bits_per_pixel: 32,
        red_bits: 8,
        green_bits: 8,
        blue_bits: 8,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    }
}

/// Independent little-endian fixture: image then one available memory region.
fn fixture() -> Vec<u8> {
    vec![
        24, 0, 0, 0, 1, 0, 0, 0, 0, 0x10, 0, 0, 0, 0, 0, 0, 0, 0x30, 0, 0, 0, 0, 0, 0, 28, 0, 0, 0,
        2, 0, 0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0x20, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
    ]
}

#[test]
fn decodes_independent_fixture_without_alignment_requirement() {
    let mut unaligned = vec![0xff];
    unaligned.extend_from_slice(&fixture());
    let info = BootInfo::parse(&unaligned[1..]).unwrap();
    assert_eq!(info.image(), 0x1000..0x3000);
    assert_eq!(info.framebuffer(), None);
    assert_eq!(info.memory_regions().collect::<Vec<_>>(), vec![region()]);
}

#[test]
fn encoding_is_deterministic_and_does_not_copy_struct_padding() {
    let mut storage = [0xa5; 128];
    let mut builder = Builder::new(&mut storage);
    builder.image(0x1000..0x3000).unwrap();
    builder.memory_region(region()).unwrap();
    builder.framebuffer(framebuffer()).unwrap();
    let info = builder.finish().unwrap();
    assert_eq!(info.framebuffer(), Some(framebuffer()));
    let mut expected = fixture();
    expected.extend_from_slice(&[
        43, 0, 0, 0, 12, 0, 0, 0, 0x78, 0x56, 0x34, 0x12, 0, 0, 0, 0, 32, 0, 0, 0, 0, 0, 0, 0, 3,
        0, 0, 0, 2, 0, 0, 0, 16, 0, 0, 0, 32, 8, 8, 8, 16, 8, 0,
    ]);
    assert_eq!(&storage[..expected.len()], &expected);
    assert!(storage[expected.len()..].iter().all(|byte| *byte == 0xa5));
}

#[test]
fn rejects_truncation_and_invalid_lengths_including_trailing_garbage() {
    let valid = fixture();
    for end in 0..valid.len() {
        assert!(BootInfo::parse(&valid[..end]).is_err(), "prefix {end}");
    }
    for length in [0_u32, 1, 7, u32::MAX] {
        let mut bytes = valid.clone();
        bytes[..4].copy_from_slice(&length.to_le_bytes());
        assert!(BootInfo::parse(&bytes).is_err());
    }
    let mut bytes = valid;
    bytes.push(0);
    assert_eq!(BootInfo::parse(&bytes).unwrap_err(), Error::Truncated);
}

#[test]
fn distinguishes_unknown_optional_records_duplicates_and_missing_requirements() {
    let mut bytes = fixture();
    bytes.extend_from_slice(&[9, 0, 0, 0, 0xff, 0, 0, 0, 42]);
    assert_eq!(BootInfo::parse(&bytes).unwrap().memory_regions().count(), 1);
    bytes.extend_from_slice(&fixture()[..24]);
    assert_eq!(BootInfo::parse(&bytes).unwrap_err(), Error::DuplicateRecord);
    assert_eq!(
        BootInfo::parse(&fixture()[..24]).unwrap_err(),
        Error::MissingRequiredRecord
    );
    assert_eq!(
        BootInfo::parse(&fixture()[24..]).unwrap_err(),
        Error::MissingRequiredRecord
    );
}

#[test]
fn capacity_failure_preserves_prior_records_and_surrounding_bytes() {
    let mut storage = [0x5a; 54];
    let mut builder = Builder::new(&mut storage[1..53]);
    builder.image(0x1000..0x3000).unwrap();
    builder.memory_region(region()).unwrap();
    assert_eq!(builder.framebuffer(framebuffer()), Err(Error::Capacity));
    assert_eq!(builder.finish().unwrap().framebuffer(), None);
    assert_eq!(&storage[1..53], fixture());
    assert_eq!((storage[0], storage[53]), (0x5a, 0x5a));
}

#[test]
fn rejects_invalid_ranges_memory_kinds_and_framebuffer_geometry() {
    let mut bytes = fixture();
    bytes[48] = 2;
    assert_eq!(BootInfo::parse(&bytes).unwrap_err(), Error::InvalidRecord);
    let mut bytes = fixture();
    bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(BootInfo::parse(&bytes).unwrap_err(), Error::InvalidRange);
    let mut bytes = fixture();
    bytes[40..48].fill(0);
    assert_eq!(BootInfo::parse(&bytes).unwrap_err(), Error::InvalidRange);

    let mut storage = [0; 256];
    let mut builder = Builder::new(&mut storage);
    assert_eq!(builder.image(2..2), Err(Error::InvalidRange));
    let mut invalid = framebuffer();
    invalid.stride_bytes = 11;
    assert_eq!(builder.framebuffer(invalid), Err(Error::InvalidFramebuffer));
    invalid = framebuffer();
    invalid.byte_len = 31;
    assert_eq!(builder.framebuffer(invalid), Err(Error::InvalidFramebuffer));
    invalid = framebuffer();
    invalid.red_shift = 8;
    assert_eq!(builder.framebuffer(invalid), Err(Error::InvalidFramebuffer));
}

#[test]
fn roundtrips_reserved_regions_and_rejects_duplicate_framebuffers() {
    let mut storage = [0; 256];
    let mut builder = Builder::new(&mut storage);
    builder.image(0x1000..0x3000).unwrap();
    let reserved = MemoryRegion {
        kind: MemoryKind::Reserved,
        ..region()
    };
    builder.memory_region(reserved).unwrap();
    builder.framebuffer(framebuffer()).unwrap();
    builder.framebuffer(framebuffer()).unwrap();
    assert_eq!(builder.finish().unwrap_err(), Error::DuplicateRecord);

    let mut builder = Builder::new(&mut storage);
    builder.image(0x1000..0x3000).unwrap();
    builder.memory_region(reserved).unwrap();
    assert_eq!(
        builder
            .finish()
            .unwrap()
            .memory_regions()
            .collect::<Vec<_>>(),
        vec![reserved]
    );
}

#[test]
fn debug_macro_has_context_and_erases_argument_evaluation_in_release() {
    diagnostics::init();
    let calls = std::cell::Cell::new(0);
    diagnostics::dbg_println!("value={}", {
        calls.set(calls.get() + 1);
        42
    });
    diagnostics::dbg_println!();
    let output =
        arch::serial::OUTPUT.with(|output| String::from_utf8(output.borrow().clone()).unwrap());
    if cfg!(debug_assertions) {
        assert_eq!(calls.get(), 1);
        assert!(output.contains("[debug kernel_contracts tests/kernel_contracts.rs:"));
        assert!(output.contains("value=42\r\n"));
        assert_eq!(output.lines().count(), 2);
    } else {
        assert_eq!(calls.get(), 0);
        assert!(output.is_empty());
    }
}

#[test]
fn fatal_output_survives_both_profiles() {
    diagnostics::init();
    assert!(std::panic::catch_unwind(|| diagnostics::fatal(format_args!("test failure"))).is_err());
    arch::serial::OUTPUT.with(|output| {
        assert_eq!(&*output.borrow(), b"re-mach: fatal: test failure\r\n");
    });
}
