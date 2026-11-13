// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! MIG's server stubs, generated from a subsystem's routines.

use core::marker::PhantomData;

use crate::ipc::MessageType;
use crate::ipc::kernel_message::Header;
use crate::ipc::port::{PortName, PortType};
use crate::slab::CacheBox;
use crate::vm::map::MapCopy;

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
    // The inline bit, bit 29.
    word(name, size, 1 << 5, number)
}

/// MIG's type word for `number` items of type `name`, `size` bits each,
/// with `flags` from bit 24.
const fn word(name: MessageType, size: u16, flags: u8, number: u32) -> u64 {
    let [size_low, size_high] = size.to_le_bytes();
    let [number_0, number_1, number_2, number_3] = number.to_le_bytes();
    // The name, the size, then the flags; the number above.
    u64::from_le_bytes([
        name.number(),
        size_low,
        size_high,
        flags,
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

/// An element of MIG's arrays.
///
/// # Safety
///
/// As `Inline`.
pub(super) unsafe trait Element: Copy {
    /// Its type name.
    const NAME: MessageType;
    /// Its size, in bits.
    const SIZE: u16;

    /// MIG's type word for `number` of them inline.
    fn word(number: u32) -> u64 {
        type_word(Self::NAME, Self::SIZE, number)
    }

    /// MIG's type word for `number` of them out of line.
    fn out_of_line_word(number: u32) -> u64 {
        // The long-form bit, bit 30; not inline.
        word(Self::NAME, Self::SIZE, 1 << 6, number)
    }
}

/// An out-of-line array a routine gives: the copy holding it, and how many
/// elements.
pub(super) struct OutOfLine<T> {
    /// The copy holding it.
    copy: CacheBox<MapCopy>,
    /// How many elements.
    count: usize,
    /// Their type.
    element: PhantomData<T>,
}

impl<T: Element> OutOfLine<T> {
    /// `count` elements in `copy`.
    pub(super) const fn new(copy: CacheBox<MapCopy>, count: usize) -> Self {
        Self {
            copy,
            count,
            element: PhantomData,
        }
    }

    /// Its type word, and the copy's address, which the reply takes.
    ///
    /// # Panics
    ///
    /// If the count is past 32 bits.
    pub(super) fn into_parts(self) -> (u64, usize) {
        let word = T::out_of_line_word(u32::try_from(self.count).unwrap());
        (
            word,
            CacheBox::into_raw(self.copy).as_ptr().expose_provenance(),
        )
    }
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
    const NAME: MessageType = MessageType::Integer32;
    const SIZE: u16 = 32;
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Element for PortName {
    const NAME: MessageType = MessageType::PortName;
    const SIZE: u16 = 32;
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Element for PortType {
    const NAME: MessageType = MessageType::Integer32;
    const SIZE: u16 = 32;
}

/// Generates a subsystem's stubs, `stub::<routine>`, and `routine(id)`: its
/// first id, then each routine as its definition reads, calling the function
/// of its name.
macro_rules! subsystem {
    (
        base $base:literal;
        $(
            $offset:literal $name:ident ($port:ident = $intran:path $(, $($arguments:tt)+)?)
            $(-> ($($outs:tt)+))?;
        )*
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
                    @sort $name ($port = $intran) [] [] [$($($arguments)+)?] [$($($outs)+)?]
                );
            )*
        }
    };
    // Sorts the arguments, in order, into ins, the request's fields, and outs,
    // the reply's, each `(name kind parameters)`.
    // An in-argument.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*]
        [$in:ident: $type:ty $(, $($rest:tt)*)?] $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($in inline $type)] [$($outs)*] [$($($rest)*)?] $out_list
        );
    };
    // An inline variable out-array of at most the request's count: the count
    // is also an in, after the others, as the array's buffer.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] []
        [$out:ident: [$element:ty; ..$max:literal], CountInOut]
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($out count $element, $max)]
            [$($outs)* ($out count $element, $max)] [] []
        );
    };
    // An out-of-line out-array.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] []
        [$out:ident: OutOfLine<$element:ty> $(, $($rest:tt)*)?]
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out out_of_line $element)] []
            [$($($rest)*)?]
        );
    };
    // One fixed inline out-argument.
    (@sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] [] [$out:ident: $type:ty]) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out inline $type)] [] []
        );
    };
    // Sorted: the stub, given the names of the locals the rules reach.
    (@sort $name:ident ($port:ident = $intran:path) [$($ins:tt)*] [$($outs:tt)*] [] []) => {
        $crate::ipc_kobject::mig::subsystem!(
            @stub $name ($port = $intran) [$($ins)*] [$($outs)*] request reply
        );
    };
    // The stub: one skeleton, each argument through its kind's rules.
    (
        @stub $name:ident ($port:ident = $intran:path)
        [$(($in:ident $in_kind:ident $($in_parameter:tt)*))*]
        [$(($out:ident $out_kind:ident $($out_parameter:tt)*))*]
        $request:ident $reply:ident
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
                $($in: mig::subsystem!(@field request $in_kind $($in_parameter)*),)*
            }

            /// The reply, as MIG lays it out.
            #[repr(C)]
            struct Reply {
                head: mig::ReplyHeader,
                $($out: mig::subsystem!(@field reply $out_kind $($out_parameter)*),)*
            }

            /// Serves `request`, as MIG's stub does.
            ///
            /// # Panics
            ///
            /// On a request MIG refuses: not yet; as the routine.
            pub fn serve(request: &KernelMessage, reply: &mut KernelMessage) {
                // SAFETY: `Request` is `repr(C)`, a `Header`, then `Inline` data only.
                let $request = unsafe { request.layout::<Request>() };
                assert!(
                    usize::try_from($request.header.size) == Ok(size_of::<Request>())
                        && !$request.header.bits.is_complex()
                        $(&& mig::subsystem!(@check $request $in $in_kind $($in_parameter)*))*,
                    concat!(stringify!($name), ": MIG_BAD_ARGUMENTS, not yet")
                );
                // SAFETY: copied in, the destination is a port's exposed address, and
                // the request holds a reference to it.
                let port =
                    unsafe { &*ptr::with_exposed_provenance::<Port>($request.header.remote_port) };
                // SAFETY: `Reply` is `repr(C)`, a `ReplyHeader`, then `Inline` or
                // `Element` data, or copies' addresses, only.
                let $reply = unsafe { reply.layout_mut::<Reply>() };
                // The intran's value drops right after the call.
                let result: Result<
                    mig::subsystem!(
                        @type $(mig::subsystem!(@value $out_kind $($out_parameter)*)),*
                    ),
                    $crate::error::Error,
                > = {
                    let $port = $intran(port);
                    super::super::$name(
                        $port.as_deref()
                        $(, mig::subsystem!(@pass $request $reply $in $in_kind $($in_parameter)*))*
                    )
                };
                mig::subsystem! { @reply $reply result $(($out $out_kind $($out_parameter)*))* }
            }
        }
    };
    // Each kind's field: in the request, then in the reply.
    (@field request inline $type:ty) => { mig::Argument<$type> };
    (@field request count $element:ty, $max:literal) => { mig::Argument<u32> };
    (@field reply inline $type:ty) => { mig::Argument<$type> };
    (@field reply count $element:ty, $max:literal) => { mig::Argument<[$element; $max]> };
    (@field reply out_of_line $element:ty) => { mig::Argument<usize> };
    // Its check: the word, whole.
    (@check $request:ident $in:ident inline $type:ty) => {
        $request.$in.word == <$type as mig::Inline>::WORD
    };
    (@check $request:ident $in:ident count $element:ty, $max:literal) => {
        $request.$in.word == <u32 as mig::Inline>::WORD
    };
    // What the routine gets: the data; for a count, the reply's array, cut to it.
    (@pass $request:ident $reply:ident $in:ident inline $type:ty) => { $request.$in.data };
    (@pass $request:ident $reply:ident $in:ident count $element:ty, $max:literal) => {
        &mut $reply.$in.data[..usize::try_from($request.$in.data).unwrap().min($max)]
    };
    // What the routine gives: the data; for a count array, how many it
    // filled; for an out-of-line array, its copy.
    (@value inline $type:ty) => { $type };
    (@value count $element:ty, $max:literal) => { usize };
    (@value out_of_line $element:ty) => { mig::OutOfLine<$element> };
    // The routine's success: nothing, its out, or its outs.
    (@type) => { () };
    (@type $value:ty) => { $value };
    (@type $($values:ty),+) => { ($($values),+) };
    // The reply with no outs: the code alone.
    (@reply $reply:ident $result:ident) => {
        $reply.head.return_code.data = $result.map_or_else(u32::from, |()| 0);
    };
    // With an out: a failure's code; else 0, the size, then the out.
    (@reply $reply:ident $result:ident ($out:ident $kind:ident $($parameter:tt)*)) => {
        match $result {
            Err(error) => $reply.head.return_code.data = u32::from(error),
            Ok($out) => {
                $reply.head.return_code.data = 0;
                $reply.head.header.size =
                    u32::try_from(mig::subsystem!(@size $out $kind $($parameter)*)).unwrap();
                mig::subsystem! { @pack $reply $out $kind $($parameter)* }
            }
        }
    };
    // With several outs, none a count array: a failure's code; else 0, the
    // whole size, then each out.
    (@reply $reply:ident $result:ident $(($out:ident $kind:ident $($parameter:tt)*))+) => {
        match $result {
            Err(error) => $reply.head.return_code.data = u32::from(error),
            Ok(($($out),+)) => {
                $reply.head.return_code.data = 0;
                $reply.head.header.size = u32::try_from(size_of::<Reply>()).unwrap();
                $(mig::subsystem! { @pack $reply $out $kind $($parameter)* })+
            }
        }
    };
    // Packing an out: the word, then the data.
    (@pack $reply:ident $out:ident inline $type:ty) => {
        $reply.$out.word = <$type as mig::Inline>::WORD;
        $reply.$out.data = $out;
    };
    // A count array's word.
    (@pack $reply:ident $out:ident count $element:ty, $max:literal) => {
        $reply.$out.word = <$element as mig::Element>::word(u32::try_from($out).unwrap());
    };
    // An out-of-line array makes the reply complex; its word, then the copy.
    (@pack $reply:ident $out:ident out_of_line $element:ty) => {
        $reply.head.header.bits = $reply.head.header.bits.with_complex();
        ($reply.$out.word, $reply.$out.data) = $out.into_parts();
    };
    // The reply's size: whole; with a count array, the elements used, padded to 8 bytes.
    (@size $out:ident inline $type:ty) => { size_of::<Reply>() };
    (@size $out:ident count $element:ty, $max:literal) => {
        core::mem::offset_of!(Reply, $out.data) + (size_of::<$element>() * $out).next_multiple_of(8)
    };
}

pub(super) use subsystem;
