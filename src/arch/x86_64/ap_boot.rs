// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Application processor startup.

use core::arch::{global_asm, naked_asm};
use core::hint;
use core::mem::offset_of;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use raw_cpuid::ExtendedFeatures;
use x86_64::VirtAddr;
use x86_64::instructions::tlb;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::model_specific::{Efer, EferFlags};
use x86_64::registers::rflags::{self, RFlags};
use x86_64::structures::gdt::DescriptorFlags;

use super::address::PhysAddr;
use super::boot_header::IA32_EFER;
use super::memory_layout::PAGE_SIZE;
use super::per_cpu::{self, PerCpu};
use super::phys_map::{self, PhysMap};
use super::{apic, counter, cpuid, fpu, gdt, idt, syscall, tss};
use crate::config::MAX_CPUS;
use crate::println;
use crate::smp::{self, MachinePerCpu as _};
use crate::sync::{CriticalSection, OnceLock};
use crate::vm::phys_map::MachinePhysMap as _;
use crate::{startup, vm};

/// Physical address of the AP boot page, once boot memory sets it aside.
pub static ADDRESS: OnceLock<PhysAddr> = OnceLock::new();

/// The AP GDT's selectors: 64-bit code, data, 32-bit code.
const CODE64_SELECTOR: u16 = 0x8;
const DATA_SELECTOR: u16 = 0x10;
const CODE32_SELECTOR: u16 = 0x18;

/// How long an AP gets to park, in nanoseconds.
const START_TIMEOUT: u64 = 5_000_000_000;

/// Where an AP is in its startup.
enum State {
    /// Not started.
    Off,
    /// The boot CPU started it and waits for it.
    Starting,
    /// Set up, waiting to be released.
    Parked,
    /// Released: it launches its first thread, and the boot CPU waits for
    /// it.
    Released,
    /// Up, in `smp::STARTED` and flushed: the boot CPU goes on.
    Running,
}

impl State {
    /// Its number.
    const fn number(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Starting => 1,
            Self::Parked => 2,
            Self::Released => 3,
            Self::Running => 4,
        }
    }
}

/// Each CPU's `State`, by number.
static STATES: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(State::Off.number()) }; MAX_CPUS];

/// What the AP being started reads; APs start one at a time.
#[repr(C)]
struct Parameters {
    /// Its CPU number.
    cpu: AtomicUsize,
    /// Its boot stack's top: the stack of its first bound thread, idle/n,
    /// a page below its top.
    stack: AtomicUsize,
    /// CR3: the kernel's PML4 table.
    page_directory: AtomicU64,
    /// The EFER bits it sets before CR3: the boot CPU's NXE.
    no_execute: AtomicU32,
    /// Its IST 1's top: the double fault's and the NMI's stack.
    double_fault: AtomicUsize,
    /// Its IST 2's top: the debug exception's stack.
    debug: AtomicUsize,
}

static PARAMETERS: Parameters = Parameters {
    cpu: AtomicUsize::new(0),
    stack: AtomicUsize::new(0),
    page_directory: AtomicU64::new(0),
    no_execute: AtomicU32::new(0),
    double_fault: AtomicUsize::new(0),
    debug: AtomicUsize::new(0),
};

unsafe extern "C" {
    /// The 16-bit stub, which `start_other_cpus` copies to the AP boot
    /// page.
    safe static ap_stub: u8;
    /// Past its last byte.
    safe static ap_stub_end: u8;
}

// An AP starts at `ap_stub`, copied to the AP boot page, in real mode with
// CS the page's segment and IP 0. The stub loads `ap_gdt` and enters
// protected mode at `ap_entry32`, which runs in place in `.boot.text`, as
// `ap_entry64` does: they switch to long mode on the boot CPU's boot page
// tables, with a null IDT, then jump to `high_entry` at the kernel map base.
global_asm!(
    ".pushsection .boot.text, \"ax\"",
    ".code16",
    ".globl ap_stub",
    "ap_stub:",
    "cli",
    "movw %cs, %ax",
    "movw %ax, %ds",
    "lgdtl ap_stub_gdt_pointer - ap_stub",
    "movl %cr0, %eax",
    "orl ${cr0_pe}, %eax",
    "movl %eax, %cr0",
    "ljmpl ${code32_selector}, $ap_entry32",
    "ap_stub_gdt_pointer:",
    ".short ap_gdt_end - ap_gdt - 1",
    ".long ap_gdt",
    ".globl ap_stub_end",
    "ap_stub_end:",
    ".code32",
    "ap_entry32:",
    "movw ${data_selector}, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movl %cr4, %eax",
    "orl ${cr4_pae}, %eax",
    "movl %eax, %cr4",
    "movl ${ia32_efer}, %ecx",
    "rdmsr",
    "orl ${efer_lme}, %eax",
    "wrmsr",
    "movl $p4table, %eax",
    "movl %eax, %cr3",
    "movl %cr0, %eax",
    "orl ${cr0_pg_wp}, %eax",
    "movl %eax, %cr0",
    "lidtl ap_null_idt",
    "ljmpl ${code64_selector}, $ap_entry64",
    ".code64",
    "ap_entry64:",
    "movw ${data_selector}, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movabsq ${high_entry}, %rax",
    "jmpq *%rax",
    ".popsection",
    ".pushsection .boot.data, \"aw\"",
    ".balign 8",
    "ap_gdt:",
    ".quad 0, {code64}, {data}, {code32}",
    "ap_gdt_end:",
    // Long enough for a 64-bit IDTR.
    "ap_null_idt:",
    ".quad 0",
    ".short 0",
    ".popsection",
    cr0_pe = const Cr0Flags::PROTECTED_MODE_ENABLE.bits(),
    code32_selector = const CODE32_SELECTOR,
    data_selector = const DATA_SELECTOR,
    cr4_pae = const Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits(),
    ia32_efer = const IA32_EFER,
    efer_lme = const EferFlags::LONG_MODE_ENABLE.bits(),
    cr0_pg_wp = const Cr0Flags::PAGING.union(Cr0Flags::WRITE_PROTECT).bits(),
    code64_selector = const CODE64_SELECTOR,
    high_entry = sym high_entry,
    code64 = const DescriptorFlags::KERNEL_CODE64.bits(),
    data = const DescriptorFlags::KERNEL_DATA.bits(),
    code32 = const DescriptorFlags::KERNEL_CODE32.bits(),
    options(att_syntax),
);

/// The AP's entry at the kernel map base, still on the boot page tables:
/// NXE, then CR3 the kernel's PML4 table, then its boot stack, then
/// `setup`.
///
/// # Safety
///
/// Entered only from `ap_entry64`, by the AP `PARAMETERS` describe.
#[unsafe(naked)]
unsafe extern "sysv64" fn high_entry() -> ! {
    naked_asm!(
        // The kernel's tables map its data no-execute.
        "movl ${ia32_efer}, %ecx",
        "rdmsr",
        "orl {parameters}+{no_execute}(%rip), %eax",
        "wrmsr",
        "movq {parameters}+{page_directory}(%rip), %rax",
        "movq %rax, %cr3",
        // The stack is mapped only in the kernel's tables.
        "movq {parameters}+{stack}(%rip), %rsp",
        "movq {parameters}+{cpu}(%rip), %rdi",
        "callq {setup}",
        "ud2",
        ia32_efer = const IA32_EFER,
        parameters = sym PARAMETERS,
        no_execute = const offset_of!(Parameters, no_execute),
        page_directory = const offset_of!(Parameters, page_directory),
        stack = const offset_of!(Parameters, stack),
        cpu = const offset_of!(Parameters, cpu),
        setup = sym setup,
        options(att_syntax),
    );
}

/// Sets AP `cpu` up, from `high_entry` on its boot stack: its per-CPU data,
/// local APIC and caching, then, once the boot CPU waits for it,
/// `cpu_setup`'s page directory, paging, descriptor tables, FPU and local
/// APIC, and its clock; then parks it, interrupts off, until the boot CPU
/// releases it to launch its first thread.
///
/// # Safety
///
/// Entered once, from `high_entry`, on AP `cpu`.
unsafe extern "sysv64" fn setup(cpu: usize) -> ! {
    // SAFETY: guaranteed by the caller: this AP is CPU `cpu`, and nothing on
    // it read `cpu_id` yet.
    unsafe { per_cpu::init(cpu) };
    apic::enable_other_cpu();
    // SAFETY: an empty RFLAGS keeps DF and IF clear.
    unsafe { rflags::write(RFlags::empty()) };
    // Before the wait: with CD and NW set, the boot CPU's stores need not
    // reach this CPU.
    // SAFETY: turns caching on; paging and protection stay as they are.
    unsafe {
        Cr0::update(|flags| {
            flags.remove(Cr0Flags::CACHE_DISABLE | Cr0Flags::NOT_WRITE_THROUGH);
        });
    }
    // Its prints follow the boot CPU's.
    while STATES[cpu].load(Ordering::Acquire) != State::Starting.number() {
        hint::spin_loop();
    }
    println!("AP=({cpu}) pagedir done");
    // SAFETY: global pages where the boot CPU has them, as on it; the APs
    // have the boot CPU's features.
    unsafe { Cr4::update(|flags| flags.set(Cr4Flags::PAGE_GLOBAL, cpuid::features().has_pge())) };
    // SAFETY: the PAT MSR is this CPU's own; entries 0 to 3 keep the reset
    // values the live mappings use, and no mapping uses entries 4 to 7.
    unsafe { phys_map::set_page_attributes() };
    let features = cpuid::cpu_id().get_extended_feature_info();
    let has = |feature: fn(&ExtendedFeatures) -> bool| features.as_ref().is_some_and(feature);
    // SAFETY: the kernel runs no code at user addresses, and reaches user
    // pages only between the sites `smap::init` patched; the APs have the
    // boot CPU's features (PGE and PAT come from its leaf 1, SMEP and SMAP
    // from this CPU's own).
    unsafe {
        Cr4::update(|flags| {
            flags.set(
                Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION,
                has(ExtendedFeatures::has_smep),
            );
            flags.set(
                Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION,
                has(ExtendedFeatures::has_smap),
            );
        });
    }
    println!("AP=({cpu}) paging done");
    // init_percpu's APIC ID; per_cpu::init named its slot at entry.
    PerCpu::store_apic_id();
    let this = PerCpu::current();
    // Its tables are in its per-CPU data, its trap stacks from
    // `start_other_cpus`.
    println!("AP=({cpu}) mpdesc done");
    // SAFETY: once, on this CPU, with its own table.
    unsafe { gdt::init(this.gdt()) };
    println!("AP=({cpu}) gdt done");
    idt::load();
    println!("AP=({cpu}) idt done");
    // The shared IDT holds the interrupt vectors already.
    println!("AP=({cpu}) int done");
    // No LDT: its user segments go in the GDT, beside the system call MSRs.
    // SAFETY: once, on this CPU, after its GDT.
    unsafe { gdt::store_user_segments(this.gdt()) };
    // SAFETY: once, on this CPU, after its user segments.
    unsafe { syscall::init() };
    let top =
        |stack: &AtomicUsize| VirtAddr::new(u64::try_from(stack.load(Ordering::Relaxed)).unwrap());
    // SAFETY: once, on this CPU, after its GDT; `start_other_cpus` took
    // the two stacks for this CPU alone, and never frees them.
    unsafe {
        tss::init(this, top(&PARAMETERS.double_fault), top(&PARAMETERS.debug));
    }
    println!("AP=({cpu}) ktss done");
    fpu::init_cpu();
    apic::setup_other_cpu();
    // startrtclock, from cpu_launch_first_thread: no tick is taken before
    // the release sets IF.
    apic::start_other_cpu_timer();
    STATES[cpu].store(State::Parked.number(), Ordering::Release);
    // Acquire: the boot CPU's work before the release is seen.
    while STATES[cpu].load(Ordering::Acquire) == State::Parked.number() {
        hint::spin_loop();
    }
    startup::launch_first_thread(None)
}

/// Starts each other CPU the ACPI tables list as enabled, one at a time,
/// and waits until it parks, on the boot CPU; each boots on the stack of
/// its first bound thread, idle/n, with trap stacks of its own.
///
/// # Panics
///
/// If the ACPI step found no APIC, a trap stack can't be allocated, an AP
/// has no idle thread, or an AP does not park within `START_TIMEOUT`.
pub fn start_other_cpus() {
    let ids = apic::APIC_IDS.get().expect("start_other_cpus: no APIC IDs");
    if ids.len() == 1 {
        return;
    }
    let page = *ADDRESS.get().expect("start_other_cpus: no AP boot page");
    let stub = (&raw const ap_stub).addr();
    let length = (&raw const ap_stub_end).addr() - stub;
    // SAFETY: the stub's bytes, in `.boot.text`, to the AP boot page, which
    // boot memory set aside for it and no CPU runs yet; both through the
    // direct map, the stub shorter than a page.
    unsafe {
        page.direct_map_ptr::<u8>()
            .copy_from_nonoverlapping(PhysAddr(stub).direct_map_ptr(), length);
    }
    assert_eq!(
        smp::current().id(),
        0,
        "start_other_cpus: not on the boot CPU"
    );
    let enabled = apic::ENABLED_CPUS
        .get()
        .expect("start_other_cpus: no enabled CPUs");
    // Each AP's IST 1 and IST 2 tops, taken before the critical section.
    let allocate = || {
        vm::kernel::allocate_stack()
            .unwrap_or_else(|error| panic!("start_other_cpus: no trap stack: {error:?}"))
            .top()
    };
    let mut trap_stacks = [(0, 0); MAX_CPUS];
    for cpu in enabled.iter().filter(|&cpu| cpu != 0) {
        trap_stacks[cpu] = (allocate(), allocate());
    }
    let _section = CriticalSection::enter();
    PARAMETERS.page_directory.store(
        u64::try_from(PhysMap::kernel().pml4().0).unwrap(),
        Ordering::Relaxed,
    );
    let no_execute = Efer::read() & EferFlags::NO_EXECUTE_ENABLE;
    PARAMETERS
        .no_execute
        .store(u32::try_from(no_execute.bits()).unwrap(), Ordering::Relaxed);
    for cpu in enabled.iter().filter(|&cpu| cpu != 0) {
        let idle = smp::of(cpu)
            .processor()
            .first_bound()
            .expect("start_other_cpus: no idle thread");
        // Its first bound thread, idle/n: start_kernel_threads resumes it
        // before softclock/n.
        PARAMETERS.cpu.store(cpu, Ordering::Relaxed);
        PARAMETERS.stack.store(
            idle.machine_state().stack_top.load(Ordering::Relaxed) - PAGE_SIZE,
            Ordering::Relaxed,
        );
        let (double_fault, debug) = trap_stacks[cpu];
        PARAMETERS
            .double_fault
            .store(double_fault, Ordering::Relaxed);
        PARAMETERS.debug.store(debug, Ordering::Relaxed);
        // Its wbinvd and uncached command writes follow these stores, which
        // the AP reads once started.
        apic::start_processor(cpu, page);
        println!("Waiting for AP {cpu}");
        STATES[cpu].store(State::Starting.number(), Ordering::Release);
        let start = counter::nanoseconds();
        while STATES[cpu].load(Ordering::Acquire) != State::Parked.number() {
            assert!(
                counter::nanoseconds() - start < START_TIMEOUT,
                "start_other_cpus: AP {cpu} did not start"
            );
            hint::spin_loop();
        }
    }
    println!("BSP: Completed SMP init");
}

/// Releases each parked AP, one at a time, and waits until it runs, on the
/// boot CPU: busy, never blocking, so no other thread of the boot CPU takes
/// a token while the AP's `cpu_up` holds its sets'.
pub fn release_other_cpus() {
    // splhigh, as start_other_cpus' wait.
    let _section = CriticalSection::enter();
    for cpu in
        (1..MAX_CPUS).filter(|&cpu| STATES[cpu].load(Ordering::Relaxed) == State::Parked.number())
    {
        // Release: its first thread sees what the boot CPU did before.
        STATES[cpu].store(State::Released.number(), Ordering::Release);
        // Acquire: its `cpu_up` and join are done.
        while STATES[cpu].load(Ordering::Acquire) != State::Running.number() {
            hint::spin_loop();
        }
    }
}

/// Joins the current CPU, an AP launching its first thread, to
/// `smp::STARTED`, flushes its whole TLB, then marks it running; the boot
/// CPU joined at startup.
pub fn activate_kernel_tables() {
    let cpu = smp::current().id();
    if smp::STARTED.contains(cpu) {
        return;
    }
    // SeqCst: a sender that misses this CPU swapped its PTEs before, so the
    // flush drops the old entries.
    smp::STARTED.insert(cpu);
    let cr4 = Cr4::read();
    if cr4.contains(Cr4Flags::PAGE_GLOBAL) {
        // SAFETY: turns global pages off and back on, which only flushes
        // the TLB, global entries included.
        unsafe {
            Cr4::write(cr4 - Cr4Flags::PAGE_GLOBAL);
            Cr4::write(cr4);
        }
    } else {
        tlb::flush_all();
    }
    // Release: the boot CPU, going on, sees its sets' tokens released.
    STATES[cpu].store(State::Running.number(), Ordering::Release);
}
