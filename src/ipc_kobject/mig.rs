// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! MIG's server stubs, generated from a subsystem's routines.

use crate::ipc::MessageType;
use crate::ipc::kernel_message::Header;

/// An argument as MIG lays it out: its type word, then its data, padded to 8 bytes.
#[repr(C)]
pub(super) struct Argument<T> {
    /// Its type word.
    pub(super) word: u64,
    /// Its data.
    pub(super) data: T,
}

/// MIG's reply header, `mig_reply_header_t`.
#[repr(C)]
pub(super) struct ReplyHeader {
    /// The reply's header.
    pub(super) header: Header,
    /// The routine's return code.
    pub(super) return_code: Argument<u32>,
}

/// MIG's type word, Mach's `mach_msg_type_t`, for `number` inline items of
/// type `name`, `size` bits each.
pub(super) const fn type_word(name: MessageType, size: u16, number: u32) -> u64 {
    let [size_low, size_high] = size.to_le_bytes();
    let [number_0, number_1, number_2, number_3] = number.to_le_bytes();
    // The name, the size, then the inline bit, bit 29; the number above.
    u64::from_le_bytes([
        name.number(),
        size_low,
        size_high,
        1 << 5,
        number_0,
        number_1,
        number_2,
        number_3,
    ])
}

/// Data MIG carries inline after its type word.
///
/// # Safety
///
/// `#[repr(C)]`, `#[repr(transparent)]` or primitive, aligned to at most 8
/// bytes, every byte in a field (no padding), and any bytes make one.
pub(super) unsafe trait Inline: Copy {
    /// MIG's type word for it.
    const WORD: u64;
}

/// An element of MIG's inline arrays.
///
/// # Safety
///
/// As `Inline`.
pub(super) unsafe trait Element: Copy {
    /// MIG's type word for `number` of them.
    fn word(number: u32) -> u64;
}

/// A C string of `N` bytes, NUL-padded, MIG's `c_string[N]`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(super) struct StringC<const N: usize>(pub(super) [u8; N]);

// SAFETY: a primitive.
unsafe impl Inline for i32 {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 1);
}

// SAFETY: a primitive.
unsafe impl Inline for u32 {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 1);
}

// SAFETY: bytes only.
unsafe impl<const N: usize> Inline for StringC<N> {
    const WORD: u64 = {
        // `N` in 32 bits, as `u32::try_from` would give it.
        assert!(N <= 0xffff_ffff, "StringC: too long");
        let [number_0, number_1, number_2, number_3, ..] = N.to_le_bytes();
        type_word(
            MessageType::StringC,
            8,
            u32::from_le_bytes([number_0, number_1, number_2, number_3]),
        )
    };
}

// SAFETY: a primitive.
unsafe impl Element for i32 {
    fn word(number: u32) -> u64 {
        type_word(MessageType::Integer32, 32, number)
    }
}

/// Generates a subsystem's stubs, `stub::<routine>`, and `routine(id)`: its
/// first id, then each routine as its definition reads, calling the function
/// of its name.
macro_rules! subsystem {
    (
        base $base:literal;
        $($offset:literal $name:ident ($($arguments:tt)*) $(-> ($($outs:tt)*))?;)*
    ) => {
        /// The routine serving a request of `id`, if one is written.
        pub(super) fn routine(
            id: i32,
        ) -> Option<
            fn(
                &$crate::ipc::kernel_message::KernelMessage,
                &mut $crate::ipc::kernel_message::KernelMessage,
            ),
        > {
            match id.wrapping_sub($base) {
                $($offset => Some(stub::$name::serve),)*
                _ => None,
            }
        }

        /// The stubs, one module per routine.
        mod stub {
            $(
                $crate::ipc_kobject::mig::subsystem!(
                    @stub $name ($($arguments)*) $(-> ($($outs)*))?
                );
            )*
        }
    };
    // No out-arguments: only a failure's code goes in the reply.
    (
        @stub $name:ident
        ($port:ident = $intran:path $(, $argument:ident: $argument_type:ty)*)
    ) => {
        /// The routine's stub.
        pub(super) mod $name {
            use core::ptr;

            // The subsystem's names, for the declaration's types and intrans.
            use super::super::*;
            use $crate::ipc::kernel_message::{Header, KernelMessage};
            use $crate::ipc::port::Port;
            use $crate::ipc_kobject::mig;

            /// The request, as MIG lays it out.
            #[repr(C)]
            struct Request {
                header: Header,
                $($argument: mig::Argument<$argument_type>,)*
            }

            /// The reply, as MIG lays it out.
            #[repr(C)]
            struct Reply {
                head: mig::ReplyHeader,
            }

            /// Serves `request`, as MIG's stub does.
            ///
            /// # Panics
            ///
            /// On a request MIG refuses: not yet; as the routine.
            pub fn serve(request: &KernelMessage, reply: &mut KernelMessage) {
                // SAFETY: `Request` is `repr(C)`, a `Header`, then `Inline` data only.
                let request = unsafe { request.layout::<Request>() };
                assert!(
                    usize::try_from(request.header.size) == Ok(size_of::<Request>())
                        && !request.header.bits.is_complex()
                        $(&& request.$argument.word == <$argument_type as mig::Inline>::WORD)*,
                    concat!(stringify!($name), ": MIG_BAD_ARGUMENTS, not yet")
                );
                // SAFETY: copied in, the destination is a port's exposed address, and
                // the request holds a reference to it.
                let port =
                    unsafe { &*ptr::with_exposed_provenance::<Port>(request.header.remote_port) };
                // SAFETY: `Reply` is `repr(C)`, a `ReplyHeader`, then `Inline` data only.
                let reply = unsafe { reply.layout_mut::<Reply>() };
                let $port = $intran(port);
                let result: Result<(), $crate::error::Error> =
                    super::super::$name($port $(, request.$argument.data)*);
                reply.head.return_code.data = result.map_or_else(u32::from, |()| 0);
            }
        }
    };
    // An inline variable out-array of at most the request's count.
    (
        @stub $name:ident
        ($port:ident = $intran:path $(, $argument:ident: $argument_type:ty)*)
        -> ($out:ident: [$element:ty; ..$max:literal], CountInOut)
    ) => {
        /// The routine's stub.
        pub(super) mod $name {
            use core::ptr;

            // The subsystem's names, for the declaration's types and intrans.
            use super::super::*;
            use $crate::ipc::kernel_message::{Header, KernelMessage};
            use $crate::ipc::port::Port;
            use $crate::ipc_kobject::mig;

            /// The request, as MIG lays it out.
            #[repr(C)]
            struct Request {
                header: Header,
                $($argument: mig::Argument<$argument_type>,)*
                count: mig::Argument<u32>,
            }

            /// The reply, as MIG lays it out.
            #[repr(C)]
            struct Reply {
                head: mig::ReplyHeader,
                $out: mig::Argument<[$element; $max]>,
            }

            /// Serves `request`, as MIG's stub does.
            ///
            /// # Panics
            ///
            /// On a request MIG refuses: not yet; as the routine.
            pub fn serve(request: &KernelMessage, reply: &mut KernelMessage) {
                // SAFETY: `Request` is `repr(C)`, a `Header`, then `Inline` data only.
                let request = unsafe { request.layout::<Request>() };
                assert!(
                    usize::try_from(request.header.size) == Ok(size_of::<Request>())
                        && !request.header.bits.is_complex()
                        $(&& request.$argument.word == <$argument_type as mig::Inline>::WORD)*
                        && request.count.word == <u32 as mig::Inline>::WORD,
                    concat!(stringify!($name), ": MIG_BAD_ARGUMENTS, not yet")
                );
                // SAFETY: copied in, the destination is a port's exposed address, and
                // the request holds a reference to it.
                let port =
                    unsafe { &*ptr::with_exposed_provenance::<Port>(request.header.remote_port) };
                // SAFETY: `Reply` is `repr(C)`, a `ReplyHeader`, then `Element` data only.
                let reply = unsafe { reply.layout_mut::<Reply>() };
                let count = usize::try_from(request.count.data).unwrap().min($max);
                let $port = $intran(port);
                let result: Result<usize, $crate::error::Error> = super::super::$name(
                    $port,
                    $(request.$argument.data,)*
                    &mut reply.$out.data[..count],
                );
                match result {
                    Err(error) => reply.head.return_code.data = u32::from(error),
                    Ok(count) => {
                        reply.head.return_code.data = 0;
                        reply.$out.word =
                            <$element as mig::Element>::word(u32::try_from(count).unwrap());
                        // The elements used, padded to 8 bytes.
                        let size = core::mem::offset_of!(Reply, $out.data)
                            + (size_of::<$element>() * count).next_multiple_of(8);
                        reply.head.header.size = u32::try_from(size).unwrap();
                    }
                }
            }
        }
    };
    // One fixed inline out-argument.
    (
        @stub $name:ident
        ($port:ident = $intran:path $(, $argument:ident: $argument_type:ty)*)
        -> ($out:ident: $out_type:ty)
    ) => {
        /// The routine's stub.
        pub(super) mod $name {
            use core::ptr;

            // The subsystem's names, for the declaration's types and intrans.
            use super::super::*;
            use $crate::ipc::kernel_message::{Header, KernelMessage};
            use $crate::ipc::port::Port;
            use $crate::ipc_kobject::mig;

            /// The request, as MIG lays it out.
            #[repr(C)]
            struct Request {
                header: Header,
                $($argument: mig::Argument<$argument_type>,)*
            }

            /// The reply, as MIG lays it out.
            #[repr(C)]
            struct Reply {
                head: mig::ReplyHeader,
                $out: mig::Argument<$out_type>,
            }

            /// Serves `request`, as MIG's stub does.
            ///
            /// # Panics
            ///
            /// On a request MIG refuses: not yet; as the routine.
            pub fn serve(request: &KernelMessage, reply: &mut KernelMessage) {
                // SAFETY: `Request` is `repr(C)`, a `Header`, then `Inline` data only.
                let request = unsafe { request.layout::<Request>() };
                assert!(
                    usize::try_from(request.header.size) == Ok(size_of::<Request>())
                        && !request.header.bits.is_complex()
                        $(&& request.$argument.word == <$argument_type as mig::Inline>::WORD)*,
                    concat!(stringify!($name), ": MIG_BAD_ARGUMENTS, not yet")
                );
                // SAFETY: copied in, the destination is a port's exposed address, and
                // the request holds a reference to it.
                let port =
                    unsafe { &*ptr::with_exposed_provenance::<Port>(request.header.remote_port) };
                // SAFETY: `Reply` is `repr(C)`, a `ReplyHeader`, then `Inline` data only.
                let reply = unsafe { reply.layout_mut::<Reply>() };
                let $port = $intran(port);
                let result: Result<$out_type, $crate::error::Error> =
                    super::super::$name($port $(, request.$argument.data)*);
                match result {
                    Err(error) => reply.head.return_code.data = u32::from(error),
                    Ok($out) => {
                        reply.head.return_code.data = 0;
                        reply.head.header.size = u32::try_from(size_of::<Reply>()).unwrap();
                        reply.$out.word = <$out_type as mig::Inline>::WORD;
                        reply.$out.data = $out;
                    }
                }
            }
        }
    };
}

pub(super) use subsystem;
