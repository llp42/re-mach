// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

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
    // A send-once right in, which the routine keeps.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*]
        [$in:ident: SendOnceRight $(, $($rest:tt)*)?] $out_list:tt
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)* ($in kept_send_once)] [$($outs)*] [$($($rest)*)?]
            $out_list
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
    // An inline out-array of at most `max`, whose count the routine gives.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] []
        [$out:ident: [$element:ty; ..$max:literal], CountOut]
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out count_out $element, $max)] [] []
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
    // An out-of-line array of send rights out.
    (@sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] [] [$out:ident: PortArray]) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out port_array)] [] []
        );
    };
    // A send right out.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] []
        [$out:ident: Port $(, $($rest:tt)*)?]
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out send_right)] [] [$($($rest)*)?]
        );
    };
    // A send-once right out.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] []
        [$out:ident: PortOnce $(, $($rest:tt)*)?]
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out send_once)] [] [$($($rest)*)?]
        );
    };
    // A send right out, or null.
    (
        @sort $name:ident $head:tt [$($ins:tt)*] [$($outs:tt)*] []
        [$out:ident: PortMaybe $(, $($rest:tt)*)?]
    ) => {
        $crate::ipc_kobject::mig::subsystem!(
            @sort $name $head [$($ins)*] [$($outs)* ($out maybe_send)] [] [$($($rest)*)?]
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
            use $crate::ipc::kernel_message::KernelMessage;
            use $crate::ipc::kernel_message::header::Header;
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
                head: mig::reply::ReplyHeader,
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
                // `Element` data, or copies', ports' or port arrays' addresses,
                // only.
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
    (@complex kept_send_once $($rest:ident)*) => { true };
    (@complex out_of_line $($rest:ident)*) => { true };
    (@complex $kind:ident $($rest:ident)*) => { mig::subsystem!(@complex $($rest)*) };
    // A right's handle takes it out of the request, its slot cleared; null
    // gives none.
    (@take $request:ident $in:ident send_right $($intran:path)?) => {
        let $in = core::ptr::NonNull::new(core::ptr::with_exposed_provenance_mut::<
            $crate::ipc::port::port::Port,
        >(core::mem::take(&mut $request.$in.data)))
        .map(|port| {
            // SAFETY: copied in, a port's data is the exposed address of a
            // reference to it given up by `into_raw`, which the handle takes.
            let port = unsafe { $crate::reference::Reference::from_raw(port) };
            $crate::ipc::port::send_right::SendRight::new(port)
        });
    };
    // Out-of-line memory's handle takes its copy out of the request, its
    // slot cleared, with the word's number; null gives none.
    (@take $request:ident $in:ident out_of_line $element:ty) => {
        let $in = core::ptr::NonNull::new(core::ptr::with_exposed_provenance_mut::<
            $crate::vm::map::copy::MapCopy,
        >(core::mem::take(&mut $request.$in.data)))
        .map(|copy| {
            // SAFETY: copied in, out-of-line memory's data is the exposed
            // address of a `COPY_CACHE` box from `CacheBox::into_raw`, which
            // the message alone holds and the handle takes.
            let copy = unsafe {
                $crate::slab::CacheBox::from_raw(&$crate::vm::map::copy::COPY_CACHE, copy)
            };
            let count = usize::try_from($request.$in.word >> 32).unwrap();
            mig::argument::OutOfLine::<$element>::new(copy, count)
        });
    };
    (@take $request:ident $in:ident kept_send_right) => {
        mig::subsystem! { @take $request $in send_right }
    };
    // A send-once right's handle takes it out of the request, its slot
    // cleared, as `SendOnceRight` asks.
    (@take $request:ident $in:ident kept_send_once) => {
        let $in = core::ptr::NonNull::new(core::ptr::with_exposed_provenance_mut::<
            $crate::ipc::port::port::Port,
        >(core::mem::take(&mut $request.$in.data)))
        .map(|port| {
            // SAFETY: copied in, a port's data is the exposed address of a
            // reference to it given up by `into_raw`, which the handle takes.
            let port = unsafe { $crate::reference::Reference::from_raw(port) };
            $crate::ipc::port::send_right::SendOnceRight::new(port)
        });
    };
    (@take $request:ident $in:ident $kind:ident $($parameter:tt)*) => {};
    // Each kind's field: in the request, then in the reply.
    (@field request send_right $($intran:path)?) => { mig::argument::Argument<usize> };
    (@field request kept_send_right) => { mig::argument::Argument<usize> };
    (@field request kept_send_once) => { mig::argument::Argument<usize> };
    (@field request inline $type:ty) => { mig::argument::Argument<$type> };
    (@field request count $element:ty, $max:literal) => { mig::argument::Argument<u32> };
    (@field request out_of_line $element:ty) => { mig::argument::Argument<usize> };
    (@field request array $element:ty, $max:literal) => { mig::argument::Argument<[$element; 0]> };
    (@field reply inline $type:ty) => { mig::argument::Argument<$type> };
    (@field reply count $element:ty, $max:literal) => { mig::argument::Argument<[$element; $max]> };
    (@field reply count_out $element:ty, $max:literal) => {
        mig::argument::Argument<[$element; $max]>
    };
    (@field reply out_of_line $element:ty) => { mig::argument::Argument<usize> };
    (@field reply send_right) => { mig::argument::Argument<usize> };
    (@field reply send_once) => { mig::argument::Argument<usize> };
    (@field reply maybe_send) => { mig::argument::Argument<usize> };
    (@field reply port_array) => { mig::argument::Argument<usize> };
    // Its check: the word, whole, or the bits its kind checks.
    (@check $request:ident $in:ident send_right $($intran:path)?) => {
        $request.$in.word == mig::argument::type_word($crate::ipc::MessageType::PortSend, 64, 1)
    };
    (@check $request:ident $in:ident kept_send_right) => {
        mig::subsystem!(@check $request $in send_right)
    };
    (@check $request:ident $in:ident kept_send_once) => {
        $request.$in.word
            == mig::argument::type_word($crate::ipc::MessageType::PortSendOnce, 64, 1)
    };
    (@check $request:ident $in:ident inline $type:ty) => {
        $request.$in.word & <$type as mig::argument::Inline>::CHECKED
            == <$type as mig::argument::Inline>::WORD
    };
    (@check $request:ident $in:ident count $element:ty, $max:literal) => {
        $request.$in.word == <u32 as mig::argument::Inline>::WORD
    };
    (@check $request:ident $in:ident out_of_line $element:ty) => {
        $request.$in.word & mig::port_array::ARRAY_CHECKED
            == <$element as mig::argument::Element>::out_of_line_word(0)
    };
    (@check $request:ident $in:ident array $element:ty, $max:literal) => {
        $request.$in.word & mig::port_array::ARRAY_CHECKED
            == <$element as mig::argument::Element>::word(0)
            && $request.$in.word >> 32 <= $max
    };
    // What the routine gets: the data; for a right, its intran's value, or
    // its port's reference; for a kept right, its handle; for a count, the
    // reply's array, cut to it; for out-of-line memory, its handle; for an
    // inline array, the elements its routine reads.
    (@pass $request:ident $reply:ident $in:ident send_right $intran:path) => {
        $in.as_deref().and_then($intran).as_deref()
    };
    (@pass $request:ident $reply:ident $in:ident send_right) => {
        $in.as_ref().map(AsRef::as_ref)
    };
    (@pass $request:ident $reply:ident $in:ident kept_send_right) => { $in };
    (@pass $request:ident $reply:ident $in:ident kept_send_once) => { $in };
    (@pass $request:ident $reply:ident $in:ident inline $type:ty) => { $request.$in.data };
    (@pass $request:ident $reply:ident $in:ident count $element:ty, $max:literal) => {
        &mut $reply.$in.data[..usize::try_from($request.$in.data).unwrap().min($max)]
    };
    (@pass $request:ident $reply:ident $in:ident out_of_line $element:ty) => { $in };
    (@pass $request:ident $reply:ident $in:ident array $element:ty, $max:literal) => {
        &$in[..<$element as mig::argument::Element>::readable(
            usize::try_from($request.$in.word >> 32).unwrap(),
        )]
    };
    // What the routine gives: the data; for a count array, how many it
    // filled; for an out-of-line array, its copy; for an array of send
    // rights, its memory; for a send right, its port's reference.
    (@value inline $type:ty) => { $type };
    (@value count $element:ty, $max:literal) => { usize };
    (@value count_out $element:ty, $max:literal) => { (usize, [$element; $max]) };
    (@value out_of_line $element:ty) => { mig::argument::OutOfLine<$element> };
    (@value send_right) => { $crate::reference::Reference<$crate::ipc::port::port::Port> };
    (@value send_once) => {
        Option<$crate::reference::Reference<$crate::ipc::port::port::Port>>
    };
    (@value maybe_send) => {
        Option<$crate::reference::Reference<$crate::ipc::port::port::Port>>
    };
    (@value port_array) => { mig::port_array::PortArray };
    // The routine's success: nothing, its out, or its outs.
    (@type) => { () };
    (@type $value:ty) => { $value };
    (@type $($values:ty),+) => { ($($values),+) };
    // The reply with no outs: the code alone.
    (@reply $reply:ident $result:ident) => {
        $reply.head.return_code.data = $result.map_or_else(u32::from, |()| 0);
    };
    // With outs: a failure's code; else 0, each out, then the size.
    (@reply $reply:ident $result:ident $(($out:ident $kind:ident $($parameter:tt)*))+) => {
        match $result {
            Err(error) => $reply.head.return_code.data = u32::from(error),
            Ok(mig::subsystem!(@pattern $($out),+)) => {
                $reply.head.return_code.data = 0;
                $(mig::subsystem! { @pack $reply $out $kind $($parameter)* })+
                $reply.head.header.size = u32::try_from(mig::subsystem!(
                    @last_size $reply $(($out $kind $($parameter)*))+
                ))
                .unwrap();
            }
        }
    };
    // The routine's success as a pattern: its out, or its outs.
    (@pattern $out:ident) => { $out };
    (@pattern $($out:ident),+) => { ($($out),+) };
    // The reply's size: its last out's.
    (@last_size $reply:ident ($out:ident $kind:ident $($parameter:tt)*)) => {
        mig::subsystem!(@size $reply $out $kind $($parameter)*)
    };
    (@last_size $reply:ident $first:tt $($rest:tt)+) => {
        mig::subsystem!(@last_size $reply $($rest)+)
    };
    // Packing an out: the word, then the data.
    (@pack $reply:ident $out:ident inline $type:ty) => {
        $reply.$out.word = <$type as mig::argument::Inline>::WORD;
        $reply.$out.data = $out;
    };
    // A count array's word.
    (@pack $reply:ident $out:ident count $element:ty, $max:literal) => {
        $reply.$out.word = <$element as mig::argument::Element>::word(u32::try_from($out).unwrap());
    };
    // A count out-array's count, then its data.
    (@pack $reply:ident $out:ident count_out $element:ty, $max:literal) => {
        let (count, data) = $out;
        $reply.$out.word = <$element as mig::argument::Element>::word(u32::try_from(count).unwrap());
        $reply.$out.data = data;
    };
    // An out-of-line array makes the reply complex; its word, then the copy.
    (@pack $reply:ident $out:ident out_of_line $element:ty) => {
        $reply.head.header.bits = $reply.head.header.bits.with_complex();
        ($reply.$out.word, $reply.$out.data) = $out.into_parts();
    };
    // An array of send rights makes the reply complex; its word, then its
    // memory.
    (@pack $reply:ident $out:ident port_array) => {
        $reply.head.header.bits = $reply.head.header.bits.with_complex();
        ($reply.$out.word, $reply.$out.data) = $out.into_parts();
    };
    // A send right makes the reply complex; its word, then its port, whose
    // reference the reply takes.
    // A send-once right makes the reply complex; its word, then its port, or
    // null for none, whose right the reply takes.
    (@pack $reply:ident $out:ident send_once) => {
        $reply.head.header.bits = $reply.head.header.bits.with_complex();
        $reply.$out.word =
            mig::argument::type_word($crate::ipc::MessageType::PortSendOnce, 64, 1);
        $reply.$out.data = $out.map_or(0, |port| {
            $crate::reference::Reference::into_raw(port)
                .as_ptr()
                .expose_provenance()
        });
    };
    // A send right, or null for none, makes the reply complex; its word,
    // then its port, whose right the reply takes.
    (@pack $reply:ident $out:ident maybe_send) => {
        $reply.head.header.bits = $reply.head.header.bits.with_complex();
        $reply.$out.word =
            mig::argument::type_word($crate::ipc::MessageType::PortSend, 64, 1);
        $reply.$out.data = $out.map_or(0, |port| {
            $crate::reference::Reference::into_raw(port)
                .as_ptr()
                .expose_provenance()
        });
    };
    (@pack $reply:ident $out:ident send_right) => {
        $reply.head.header.bits = $reply.head.header.bits.with_complex();
        $reply.$out.word = mig::argument::type_word($crate::ipc::MessageType::PortSend, 64, 1);
        $reply.$out.data = $crate::reference::Reference::into_raw($out)
            .as_ptr()
            .expose_provenance();
    };
    // The reply's size: whole; with a count array, the elements used, padded to 8 bytes.
    (@size $reply:ident $out:ident inline $type:ty) => { size_of::<Reply>() };
    (@size $reply:ident $out:ident out_of_line $element:ty) => { size_of::<Reply>() };
    (@size $reply:ident $out:ident send_right) => { size_of::<Reply>() };
    (@size $reply:ident $out:ident send_once) => { size_of::<Reply>() };
    (@size $reply:ident $out:ident maybe_send) => { size_of::<Reply>() };
    (@size $reply:ident $out:ident port_array) => { size_of::<Reply>() };
    (@size $reply:ident $out:ident count $element:ty, $max:literal) => {
        core::mem::offset_of!(Reply, $out.data) + (size_of::<$element>() * $out).next_multiple_of(8)
    };
    (@size $reply:ident $out:ident count_out $element:ty, $max:literal) => {
        core::mem::offset_of!(Reply, $out.data)
            + (size_of::<$element>() * usize::try_from($reply.$out.word >> 32).unwrap())
                .next_multiple_of(8)
    };
}

pub(crate) use subsystem;
