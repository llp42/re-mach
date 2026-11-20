// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! MIG's server stubs, generated from a subsystem's routines.

use core::marker::PhantomData;

use crate::ipc::MessageType;
use crate::ipc::kernel_message::Header;
use crate::ipc::port::{PortName, PortType};
use crate::slab::CacheBox;
use crate::vm::VirtualAddress;
use crate::vm::map::MapCopy;
use crate::vm::object::Offset;

/// An argument as MIG lays it out: its type word, then its data, padded to 8 bytes.
#[repr(C)]
pub struct Argument<T> {
    /// Its type word.
    pub word: u64,
    /// Its data.
    pub data: T,
}

/// MIG's reply header, `mig_reply_header_t`.
#[repr(C)]
pub struct ReplyHeader {
    /// The reply's header.
    pub header: Header,
    /// The routine's return code.
    pub return_code: Argument<u32>,
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
pub unsafe trait Inline: Copy {
    /// MIG's type word for it.
    const WORD: u64;
    /// The word's bits MIG's stub checks: all of them.
    const CHECKED: u64 = u64::MAX;
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

/// An out-of-line array a routine gives or gets: the copy holding it, and
/// how many elements.
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

    /// The copy holding it.
    pub(super) fn into_copy(self) -> CacheBox<MapCopy> {
        self.copy
    }
}

/// The bits of an out-of-line item's or an inline array's type word MIG's
/// stub checks: all but the number, `msgt_deallocate` and `msgt_unused`, as
/// `_Xvm_write` and `_Xthread_set_state`.
pub(super) const ARRAY_CHECKED: u64 = !((0xffff_ffff << 32) | (1 << 31) | (0x1f << 24));

/// A boolean, MIG's `boolean_t`: true unless 0.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(super) struct Boolean(u32);

impl From<Boolean> for bool {
    fn from(boolean: Boolean) -> Self {
        boolean.0 != 0
    }
}

/// A C string of `N` bytes, NUL-padded, MIG's `c_string[N]`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(super) struct StringC<const N: usize>(pub(super) [u8; N]);

/// A device's name, MIG's `dev_name_t`: a C string of 128 bytes, in one
/// long-form item.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(super) struct DeviceName([u8; 128]);

impl DeviceName {
    /// Its bytes before the NUL MIG's stub puts at its last byte.
    pub(super) fn as_bytes(&self) -> &[u8] {
        let bytes = &self.0[..127];
        &bytes[..bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len())]
    }
}

// SAFETY: a primitive.
unsafe impl Inline for i32 {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 1);
}

// SAFETY: a primitive.
unsafe impl Inline for u32 {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 1);
}

// SAFETY: a primitive.
unsafe impl Inline for usize {
    const WORD: u64 = type_word(MessageType::Integer64, 64, 1);
}

// SAFETY: `repr(transparent)` over a `usize`.
unsafe impl Inline for VirtualAddress {
    const WORD: u64 = type_word(MessageType::Integer64, 64, 1);
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Inline for Boolean {
    const WORD: u64 = type_word(MessageType::Boolean, 32, 1);
}

// SAFETY: `repr(transparent)` over a `usize`.
unsafe impl Inline for Offset {
    const WORD: u64 = type_word(MessageType::Integer64, 64, 1);
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Inline for PortName {
    const WORD: u64 = type_word(MessageType::PortName, 32, 1);
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Inline for PortType {
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

// SAFETY: bytes only.
unsafe impl Inline for DeviceName {
    // One item of 1024 bits, inline and long-form.
    const WORD: u64 = word(MessageType::StringC, 1024, (1 << 5) | (1 << 6), 1);
    // All but msgt_deallocate and msgt_unused, as _Xdevice_open.
    const CHECKED: u64 = !((1 << 31) | (0x1f << 24));
}

// SAFETY: a primitive.
unsafe impl Element for u8 {
    const NAME: MessageType = MessageType::Byte;
    const SIZE: u16 = 8;
}

// SAFETY: a primitive.
unsafe impl Element for i32 {
    const NAME: MessageType = MessageType::Integer32;
    const SIZE: u16 = 32;
}

// SAFETY: a primitive.
unsafe impl Element for u32 {
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
            $offset:literal $name:ident $arguments:tt
            $(-> ($($outs:tt)+))?;
        )*
    ) => {
        /// The routine serving a request of `id`, if one is written.
        pub(super) fn routine(
            id: i32,
        ) -> Option<
            fn(
                &mut $crate::ipc::kernel_message::KernelMessage,
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
                    @head $name $arguments [$($($outs)+)?]
                );
            )*
        }
    };
    // The request port: through an intran, or raw.
    (
        @head $name:ident ($port:ident = $intran:path $(, $($arguments:tt)+)?) $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name (intran $port $intran) [] [] [$($($arguments)+)?] $out_list
        );
    };
    (@head $name:ident ($port:ident: Port $(, $($arguments:tt)+)?) $out_list:tt) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name (raw $port) [] [] [$($($arguments)+)?] $out_list
        );
    };
    // Sorts the arguments, in order, into ins, the request's fields, and outs,
    // the reply's, each `(name kind parameters)`.
    // A send right in, through an intran.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*]
        [$in:ident = $intran:path $(, $($rest:tt)*)?] $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($in send_right $intran)] [$($outs)*] [$($($rest)*)?]
            $out_list
        );
    };
    // A send right in, raw.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*]
        [$in:ident: Port $(, $($rest:tt)*)?] $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($in send_right)] [$($outs)*] [$($($rest)*)?] $out_list
        );
    };
    // A send right in, which the routine keeps.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*]
        [$in:ident: SendRight $(, $($rest:tt)*)?] $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($in kept_send_right)] [$($outs)*] [$($($rest)*)?]
            $out_list
        );
    };
    // Out-of-line memory in.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*]
        [$in:ident: OutOfLine<$element:ty> $(, $($rest:tt)*)?] $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($in out_of_line $element)] [$($outs)*]
            [$($($rest)*)?] $out_list
        );
    };
    // An inline variable array in, last: its elements follow the request's
    // fields.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*]
        [$in:ident: [$element:ty; ..$max:literal]] $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($in array $element, $max)] [$($outs)*] [] $out_list
        );
    };
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
    // A send right out.
    (@sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] [] [$out:ident: Port]) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out send_right)] [] []
        );
    };
    // A fixed inline out-argument.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] []
        [$out:ident: $type:ty $(, $($rest:tt)*)?]
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out inline $type)] [] [$($($rest)*)?]
        );
    };
    // Sorted: the stub, given the names of the locals the rules reach.
    (@sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] [] []) => {
        $crate::ipc_kobject::mig::subsystem!(
            @stub $name $head [$($ins)*] [$($outs)*] request reply port
        );
    };
    // The stub: one skeleton, each argument through its kind's rules.
    (
        @stub $name:ident ($port_kind:ident $port:ident $($intran:path)?)
        [$(($in:ident $in_kind:ident $($in_parameter:tt)*))*]
        [$(($out:ident $out_kind:ident $($out_parameter:tt)*))*]
        $request:ident $reply:ident $request_port:ident
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

            /// Serves `request`, as MIG's stub does; the request's send rights
            /// and out-of-line memory are taken out of it, the rights released
            /// as the call ends unless the routine keeps them, the memory given
            /// to the routine.
            ///
            /// # Panics
            ///
            /// On a request MIG refuses: not yet; as the routine.
            pub fn serve(request: &mut KernelMessage, reply: &mut KernelMessage) {
                mig::subsystem! { @layout request $request $(($in $in_kind $($in_parameter)*))* }
                // Complex if it carries a right or memory.
                assert!(
                    usize::try_from($request.header.size)
                        == Ok(mig::subsystem!(
                            @request_size $request $(($in $in_kind $($in_parameter)*))*
                        ))
                        && $request.header.bits.is_complex()
                            == mig::subsystem!(@complex $($in_kind)*)
                        $(&& mig::subsystem!(@check $request $in $in_kind $($in_parameter)*))*,
                    concat!(stringify!($name), ": MIG_BAD_ARGUMENTS, not yet")
                );
                // SAFETY: copied in, the destination is a port's exposed address, and
                // the request holds a reference to it.
                let $request_port =
                    unsafe { &*ptr::with_exposed_provenance::<Port>($request.header.remote_port) };
                // SAFETY: `Reply` is `repr(C)`, a `ReplyHeader`, then `Inline` or
                // `Element` data, or copies' or ports' addresses, only.
                let $reply = unsafe { reply.layout_mut::<Reply>() };
                // The intrans' values and the rights drop right after the call.
                let result: Result<
                    mig::subsystem!(
                        @type $(mig::subsystem!(@value $out_kind $($out_parameter)*)),*
                    ),
                    $crate::error::Error,
                > = {
                    $(mig::subsystem! { @take $request $in $in_kind $($in_parameter)* })*
                    let $port = mig::subsystem!(@port $request_port $port_kind $($intran)?);
                    super::super::$name(
                        mig::subsystem!(@port_pass $port $port_kind)
                        $(, mig::subsystem!(@pass $request $reply $in $in_kind $($in_parameter)*))*
                    )
                };
                mig::subsystem! { @reply $reply result $(($out $out_kind $($out_parameter)*))* }
            }
        }
    };
    // The request in place: its fields; with an inline array, last, also the
    // rest of the buffer, the array's room.
    (@layout $message:ident $request:ident) => {
        // SAFETY: `Request` is `repr(C)`, a `Header`, then `Inline` data, or
        // copies' or ports' addresses, only.
        let $request = unsafe { $message.layout_mut::<Request>() };
    };
    (@layout $message:ident $request:ident ($in:ident array $element:ty, $max:literal)) => {
        // SAFETY: `Request` is `repr(C)`, a `Header`, then `Inline` data,
        // type words, or copies' or ports' addresses, only; the array's
        // elements are `Element`s.
        let ($request, $in) = unsafe { $message.split_layout_mut::<Request, $element>() };
    };
    (
        @layout $message:ident $request:ident ($in:ident $kind:ident $($parameter:tt)*)
        $($rest:tt)*
    ) => {
        mig::subsystem! { @layout $message $request $($rest)* }
    };
    // The request's size: its fields, then an inline array's elements, padded
    // to 8 bytes.
    (@request_size $request:ident) => { size_of::<Request>() };
    (@request_size $request:ident ($in:ident array $element:ty, $max:literal)) => {
        size_of::<Request>()
            + (size_of::<$element>() * usize::try_from($request.$in.word >> 32).unwrap())
                .next_multiple_of(8)
    };
    (
        @request_size $request:ident ($in:ident $kind:ident $($parameter:tt)*) $($rest:tt)*
    ) => {
        mig::subsystem!(@request_size $request $($rest)*)
    };
    // The request port as the routine gets it: through its intran, or raw.
    (@port $request_port:ident intran $intran:path) => { $intran($request_port) };
    (@port $request_port:ident raw) => { $request_port };
    (@port_pass $port:ident intran) => { $port.as_ref() };
    (@port_pass $port:ident raw) => { $port };
    // Whether the request is complex: if any in is a right or memory.
    (@complex) => { false };
    (@complex send_right $($rest:ident)*) => { true };
    (@complex kept_send_right $($rest:ident)*) => { true };
    (@complex out_of_line $($rest:ident)*) => { true };
    (@complex $kind:ident $($rest:ident)*) => { mig::subsystem!(@complex $($rest)*) };
    // A right's handle takes it out of the request, its slot cleared; null
    // gives none.
    (@take $request:ident $in:ident send_right $($intran:path)?) => {
        let $in = core::ptr::NonNull::new(core::ptr::with_exposed_provenance_mut::<
            $crate::ipc::port::Port,
        >(core::mem::take(&mut $request.$in.data)))
        .map(|port| {
            // SAFETY: copied in, a port's data is the exposed address of a
            // reference to it given up by `into_raw`, which the handle takes.
            let port = unsafe { $crate::reference::Reference::from_raw(port) };
            $crate::ipc::port::SendRight::new(port)
        });
    };
    // Out-of-line memory's handle takes its copy out of the request, its
    // slot cleared, with the word's number; null gives none.
    (@take $request:ident $in:ident out_of_line $element:ty) => {
        let $in = core::ptr::NonNull::new(core::ptr::with_exposed_provenance_mut::<
            $crate::vm::map::MapCopy,
        >(core::mem::take(&mut $request.$in.data)))
        .map(|copy| {
            // SAFETY: copied in, out-of-line memory's data is the exposed
            // address of a `COPY_CACHE` box from `CacheBox::into_raw`, which
            // the message alone holds and the handle takes.
            let copy = unsafe {
                $crate::slab::CacheBox::from_raw(&$crate::vm::map::COPY_CACHE, copy)
            };
            let count = usize::try_from($request.$in.word >> 32).unwrap();
            mig::OutOfLine::<$element>::new(copy, count)
        });
    };
    (@take $request:ident $in:ident kept_send_right) => {
        mig::subsystem! { @take $request $in send_right }
    };
    (@take $request:ident $in:ident $kind:ident $($parameter:tt)*) => {};
    // Each kind's field: in the request, then in the reply.
    (@field request send_right $($intran:path)?) => { mig::Argument<usize> };
    (@field request kept_send_right) => { mig::Argument<usize> };
    (@field request inline $type:ty) => { mig::Argument<$type> };
    (@field request count $element:ty, $max:literal) => { mig::Argument<u32> };
    (@field request out_of_line $element:ty) => { mig::Argument<usize> };
    (@field request array $element:ty, $max:literal) => { mig::Argument<[$element; 0]> };
    (@field reply inline $type:ty) => { mig::Argument<$type> };
    (@field reply count $element:ty, $max:literal) => { mig::Argument<[$element; $max]> };
    (@field reply out_of_line $element:ty) => { mig::Argument<usize> };
    (@field reply send_right) => { mig::Argument<usize> };
    // Its check: the word, whole, or the bits its kind checks.
    (@check $request:ident $in:ident send_right $($intran:path)?) => {
        $request.$in.word == mig::type_word($crate::ipc::MessageType::PortSend, 64, 1)
    };
    (@check $request:ident $in:ident kept_send_right) => {
        mig::subsystem!(@check $request $in send_right)
    };
    (@check $request:ident $in:ident inline $type:ty) => {
        $request.$in.word & <$type as mig::Inline>::CHECKED == <$type as mig::Inline>::WORD
    };
    (@check $request:ident $in:ident count $element:ty, $max:literal) => {
        $request.$in.word == <u32 as mig::Inline>::WORD
    };
    (@check $request:ident $in:ident out_of_line $element:ty) => {
        $request.$in.word & mig::ARRAY_CHECKED
            == <$element as mig::Element>::out_of_line_word(0)
    };
    (@check $request:ident $in:ident array $element:ty, $max:literal) => {
        $request.$in.word & mig::ARRAY_CHECKED == <$element as mig::Element>::word(0)
            && $request.$in.word >> 32 <= $max
    };
    // What the routine gets: the data; for a right, its intran's value, or
    // its port's reference; for a kept right, its handle; for a count, the
    // reply's array, cut to it; for out-of-line memory, its handle; for an
    // inline array, its elements.
    (@pass $request:ident $reply:ident $in:ident send_right $intran:path) => {
        $in.as_deref().and_then($intran).as_deref()
    };
    (@pass $request:ident $reply:ident $in:ident send_right) => {
        $in.as_ref().map(AsRef::as_ref)
    };
    (@pass $request:ident $reply:ident $in:ident kept_send_right) => { $in };
    (@pass $request:ident $reply:ident $in:ident inline $type:ty) => { $request.$in.data };
    (@pass $request:ident $reply:ident $in:ident count $element:ty, $max:literal) => {
        &mut $reply.$in.data[..usize::try_from($request.$in.data).unwrap().min($max)]
    };
    (@pass $request:ident $reply:ident $in:ident out_of_line $element:ty) => { $in };
    (@pass $request:ident $reply:ident $in:ident array $element:ty, $max:literal) => {
        &$in[..usize::try_from($request.$in.word >> 32).unwrap()]
    };
    // What the routine gives: the data; for a count array, how many it
    // filled; for an out-of-line array, its copy; for a send right, its
    // port's reference.
    (@value inline $type:ty) => { $type };
    (@value count $element:ty, $max:literal) => { usize };
    (@value out_of_line $element:ty) => { mig::OutOfLine<$element> };
    (@value send_right) => { $crate::reference::Reference<$crate::ipc::port::Port> };
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
    // A send right makes the reply complex; its word, then its port, whose
    // reference the reply takes.
    (@pack $reply:ident $out:ident send_right) => {
        $reply.head.header.bits = $reply.head.header.bits.with_complex();
        $reply.$out.word = mig::type_word($crate::ipc::MessageType::PortSend, 64, 1);
        $reply.$out.data = $crate::reference::Reference::into_raw($out)
            .as_ptr()
            .expose_provenance();
    };
    // The reply's size: whole; with a count array, the elements used, padded to 8 bytes.
    (@size $out:ident inline $type:ty) => { size_of::<Reply>() };
    (@size $out:ident send_right) => { size_of::<Reply>() };
    (@size $out:ident count $element:ty, $max:literal) => {
        core::mem::offset_of!(Reply, $out.data) + (size_of::<$element>() * $out).next_multiple_of(8)
    };
}

pub(crate) use subsystem;
