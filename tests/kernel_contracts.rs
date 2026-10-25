// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Host checks of the actual diagnostic macros.

#[path = "../src/diagnostics.rs"]
mod diagnostics;

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
