// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

use super::config::{self, PrepareError, SignalSafeInitConfig};
use super::fmt::{write_i32, I32_BUF_CAPACITY};
use super::policy::{
    app_handler_is_real, app_recovered, chain_action, disposition_of, is_genuine_fault,
    should_run_app_first, ChainAction,
};
use super::state::{self, sig_index, BeginInitError};
use super::sys::{self, FdSink};
use super::{backtrace, capabilities};
use super::{CrashContext, Report, SignalInfo};
use crate::signal_owner::{self, SignalOwner};

// Used only by forked children; 125 matches the existing shell-like "cannot exec" convention.
const EXIT_CODE_FAILURE: i32 = 125;
const REAP_KILL_TIMEOUT_MS: i64 = 500;
const REAP_WAIT_INTERVAL_MS: i32 = 100;
const ALT_STACK_SIZE: usize = 64 * 1024;
const ALT_STACK_GUARD_SIZE: usize = 4096;

const _: () = assert!(super::SECTION_BUF_CAPACITY <= ALT_STACK_SIZE / 8);

#[repr(C, align(4096))]
struct AltStackLayout {
    guard: [u8; ALT_STACK_GUARD_SIZE],
    usable: [u8; ALT_STACK_SIZE],
}

struct AltStackStorage(UnsafeCell<AltStackLayout>);

unsafe impl Sync for AltStackStorage {}

static ALT_STACK: AltStackStorage = AltStackStorage(UnsafeCell::new(AltStackLayout {
    guard: [0; ALT_STACK_GUARD_SIZE],
    usable: [0; ALT_STACK_SIZE],
}));

#[derive(Clone, Copy)]
struct Target {
    fn_ptr: *mut c_void,
    flags: i32,
}

#[derive(Clone, Copy)]
struct CrashEvent {
    sig: i32,
    si_code: i32,
    has_info: bool,
    si_addr: usize,
    pid: i32,
    tid: i32,
    ucontext: *mut c_void,
}

impl CrashEvent {
    fn context<'a>(self, frames: &'a [usize]) -> CrashContext<'a> {
        CrashContext {
            signal: SignalInfo::new(self.sig, self.si_code, self.si_addr, self.has_info),
            pid: self.pid,
            tid: self.tid,
            frames,
        }
    }
}

static APP_CHAIN_TID: AtomicI32 = AtomicI32::new(0);
static APP_CHAIN_STACK: AtomicUsize = AtomicUsize::new(0);

struct RepeatFaultSlot {
    pc: AtomicUsize,
    addr: AtomicUsize,
    count: AtomicUsize,
}

impl RepeatFaultSlot {
    const fn new() -> Self {
        Self {
            pc: AtomicUsize::new(0),
            addr: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
        }
    }

    fn tripped(&self, pc: usize, addr: usize) -> bool {
        if pc == 0 {
            return false;
        }

        let last_pc = self.pc.load(Ordering::Relaxed);
        let last_addr = self.addr.load(Ordering::Relaxed);
        if last_pc == pc && last_addr == addr {
            self.count.fetch_add(1, Ordering::Relaxed) + 1 >= 2
        } else {
            self.addr.store(addr, Ordering::Relaxed);
            self.count.store(1, Ordering::Relaxed);
            self.pc.store(pc, Ordering::Relaxed);
            false
        }
    }

    #[cfg(test)]
    fn reset(&self) {
        self.pc.store(0, Ordering::Relaxed);
        self.addr.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
    }
}

static REPEAT_FAULT: [RepeatFaultSlot; state::NSIG] =
    [const { RepeatFaultSlot::new() }; state::NSIG];
/// Prevents recursive crash collection. Reset only during explicit shutdown/re-init lifecycle.
static COLLECTING: AtomicBool = AtomicBool::new(false);

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitResult {
    Enabled = 0,
    Failed = 2,
    AlreadyInitialized = 3,
    OwnerConflict = 4,
    InvalidConfig = 5,
}

pub fn init_result(config: &SignalSafeInitConfig<'_>) -> InitResult {
    init_with_prepare(|| config::prepare_result(config))
}

fn init_with_prepare(prepare: impl FnOnce() -> Result<(), PrepareError>) -> InitResult {
    let begin = state::begin_init();
    if let Err(err) = begin {
        return err.into();
    }
    if !signal_owner::acquire(SignalOwner::SignalSafeCollector) {
        state::reset_init();
        return InitResult::OwnerConflict;
    }
    // Once ownership is acquired, every failure must release it and reset init state.
    let acquired = (|| {
        prepare().map_err(InitResult::from)?;
        if !install_alt_stack_if_requested() {
            return Err(InitResult::Failed);
        }
        Ok(())
    })();
    if let Err(err) = acquired {
        signal_owner::release(SignalOwner::SignalSafeCollector);
        state::reset_init();
        return err;
    }
    install_all_handlers();
    state::INSTALLED.store(true, Ordering::Release);
    state::finish_init();
    state::HANDLERS_ENABLED.store(true, Ordering::Release);
    InitResult::Enabled
}

pub fn bootstrap_complete() {
    if state::ONLY_BOOTSTRAP.load(Ordering::Relaxed) {
        shutdown();
    }
}

pub fn shutdown() {
    state::HANDLERS_ENABLED.store(false, Ordering::Release);
    uninstall_all_handlers();
    COLLECTING.store(false, Ordering::Relaxed);
    state::INSTALLED.store(false, Ordering::Release);
    signal_owner::release(SignalOwner::SignalSafeCollector);
    state::reset_init();
}

impl From<BeginInitError> for InitResult {
    fn from(err: BeginInitError) -> Self {
        match err {
            BeginInitError::AlreadyInitialized => Self::AlreadyInitialized,
            BeginInitError::Busy => Self::Failed,
        }
    }
}

impl From<PrepareError> for InitResult {
    fn from(err: PrepareError) -> Self {
        match err {
            PrepareError::InvalidConfig => Self::InvalidConfig,
            PrepareError::Failed => Self::Failed,
        }
    }
}

fn effective_target(idx: usize) -> Target {
    let (fn_ptr, flags) = state::signal_slot(idx).original_handler();
    Target { fn_ptr, flags }
}

unsafe fn invoke_handler(
    t: &Target,
    sig: c_int,
    info: *mut libc::siginfo_t,
    ucontext: *mut c_void,
) {
    if t.flags & libc::SA_SIGINFO != 0 {
        let f: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) =
            core::mem::transmute(t.fn_ptr);
        f(sig, info, ucontext);
    } else {
        let f: extern "C" fn(c_int) = core::mem::transmute(t.fn_ptr);
        f(sig);
    }
}

/// Tracks app-first handler invocation without relying on cleanup after the call.
///
/// A recovering app handler may leave this frame via siglongjmp, so a simple boolean would stay
/// set forever. Supported Unix targets use downward-growing stacks: a nested crash inside the app
/// handler has a stack address below the recorded frame, while a later signal after longjmp has
/// unwound above it. Different-thread entries skip app-first while the earlier handler is active.
fn enter_app_chain(tid: i32, stack_pos: usize) -> bool {
    let owner = APP_CHAIN_TID.load(Ordering::Relaxed);
    if owner == 0 {
        APP_CHAIN_STACK.store(stack_pos, Ordering::Relaxed);
        APP_CHAIN_TID.store(tid, Ordering::Relaxed);
        return true;
    }

    if owner != tid {
        return false;
    }

    let recorded = APP_CHAIN_STACK.load(Ordering::Relaxed);
    if stack_pos > recorded {
        APP_CHAIN_STACK.store(stack_pos, Ordering::Relaxed);
        APP_CHAIN_TID.store(tid, Ordering::Relaxed);
        true
    } else {
        false
    }
}

fn leave_app_chain(tid: i32, stack_pos: usize) {
    if APP_CHAIN_TID.load(Ordering::Relaxed) == tid
        && APP_CHAIN_STACK.load(Ordering::Relaxed) == stack_pos
    {
        APP_CHAIN_STACK.store(0, Ordering::Relaxed);
        APP_CHAIN_TID.store(0, Ordering::Relaxed);
    }
}

fn app_return_repeated_fault(idx: usize, pc: usize, addr: usize) -> bool {
    REPEAT_FAULT[idx].tripped(pc, addr)
}

fn crash_debug(msg: &[u8], sig: i32) {
    if !state::DEBUG_LOG.load(Ordering::Relaxed) {
        return;
    }
    let mut sink = FdSink::new(libc::STDERR_FILENO);
    let _ = super::Sink::put(&mut sink, b"dd-crashtracker[signal-safe]: ");
    let _ = super::Sink::put(&mut sink, msg);
    if sig >= 0 {
        let _ = super::Sink::put(&mut sink, b" ");
        let mut buf = [0u8; I32_BUF_CAPACITY];
        let written = write_i32(sig, &mut buf);
        let _ = super::Sink::put(&mut sink, &buf[..written]);
    }
    let _ = super::Sink::put(&mut sink, b"\n");
}

fn sanitize_clone(mut keep_fd: i32, close_stdio_without_devnull: bool) -> i32 {
    if (libc::STDIN_FILENO..=libc::STDERR_FILENO).contains(&keep_fd) {
        let relocated = sys::fcntl_dupfd(keep_fd, libc::STDERR_FILENO + 1);
        if relocated < 0 {
            return -1;
        }
        sys::close(keep_fd);
        keep_fd = relocated;
    }

    let _ = reset_signals_to_default(&config::CRASH_SIGNALS);
    disable_alt_stack();

    let devnull = if capabilities::has(capabilities::DEV_NULL) {
        sys::open_readwrite(c"/dev/null".as_ptr().cast())
    } else {
        -1
    };
    if devnull >= 0 {
        let _ = sys::dup2(devnull, libc::STDIN_FILENO);
        let _ = sys::dup2(devnull, libc::STDOUT_FILENO);
        let _ = sys::dup2(devnull, libc::STDERR_FILENO);
        if devnull > libc::STDERR_FILENO {
            sys::close(devnull);
        }
    } else if close_stdio_without_devnull {
        close_stdio();
    }
    keep_fd
}

fn close_stdio() {
    sys::close(libc::STDIN_FILENO);
    sys::close(libc::STDOUT_FILENO);
    sys::close(libc::STDERR_FILENO);
}

fn reset_signals_to_default(signals: &[c_int]) -> bool {
    let mut dfl: libc::sigaction = unsafe { core::mem::zeroed() };
    dfl.sa_sigaction = libc::SIG_DFL;
    unsafe {
        libc::sigemptyset(&mut dfl.sa_mask);
    }
    let mut ok = true;
    for &sig in signals {
        ok &= unsafe { libc::sigaction(sig, &dfl, null_mut()) == 0 };
    }
    ok
}

unsafe fn unblock_signal(sig: c_int) {
    let mut set: libc::sigset_t = core::mem::zeroed();
    libc::sigemptyset(&mut set);
    libc::sigaddset(&mut set, sig);
    libc::sigprocmask(libc::SIG_UNBLOCK, &set, null_mut());
}

fn install_alt_stack_if_requested() -> bool {
    if !state::CREATE_ALT_STACK.load(Ordering::Relaxed) {
        return true;
    }

    install_alt_stack_with(sys::mprotect_none, install_sigaltstack)
}

fn install_alt_stack_with(
    mprotect_none: fn(*mut u8, usize) -> bool,
    sigaltstack: fn(&libc::stack_t) -> bool,
) -> bool {
    let layout = ALT_STACK.0.get();
    let guard = unsafe { core::ptr::addr_of_mut!((*layout).guard).cast::<u8>() };
    let usable = unsafe { core::ptr::addr_of_mut!((*layout).usable).cast::<c_void>() };
    if !mprotect_none(guard, ALT_STACK_GUARD_SIZE) {
        capabilities::note_degraded(capabilities::DEGRADED_ALT_STACK_GUARD_UNAVAILABLE);
        crash_debug(b"alt stack guard unavailable", -1);
    }

    let stack = libc::stack_t {
        ss_sp: usable,
        ss_flags: 0,
        ss_size: ALT_STACK_SIZE,
    };
    sigaltstack(&stack)
}

fn install_sigaltstack(stack: &libc::stack_t) -> bool {
    unsafe { libc::sigaltstack(stack, null_mut()) == 0 }
}

/// Unregister any alternate signal stack inherited by a forked child.
///
/// The child resets its crash handlers to `SIG_DFL`, so the alt stack is no longer needed. Dropping
/// it explicitly means that even if some inherited disposition were re-armed, the child can never
/// run a handler on a stack region whose contents we no longer maintain.
fn disable_alt_stack() {
    let stack = libc::stack_t {
        ss_sp: null_mut(),
        ss_flags: libc::SS_DISABLE,
        ss_size: 0,
    };
    let _ = unsafe { libc::sigaltstack(&stack, null_mut()) };
}

/// Ignore `SIGPIPE` in a collector child before it writes the report.
///
/// The child inherits the crashing process' `SIGPIPE` disposition, which is often `SIG_DFL`
/// (terminate). If the receiver closed the read end, we want the write to fail with `EPIPE` — which
/// [`FdSink`](sys::FdSink) already reports as an error — rather than a `SIGPIPE` killing us in the
/// middle of the report.
fn ignore_sigpipe() {
    let mut ign: libc::sigaction = unsafe { core::mem::zeroed() };
    ign.sa_sigaction = libc::SIG_IGN;
    unsafe {
        libc::sigemptyset(&mut ign.sa_mask);
        let _ = libc::sigaction(libc::SIGPIPE, &ign, null_mut());
    }
}

fn strip_loader_injection_env() {
    let env = sys::environ_ptr();
    if env.is_null() {
        return;
    }
    const PREFIXES: [&[u8]; 2] = [b"LD_PRELOAD=", b"LD_AUDIT="];
    unsafe {
        let mut src = env;
        let mut dst = env;
        while !(*src).is_null() {
            let entry = *src;
            let injected = PREFIXES.iter().any(|p| sys::cstr_has_prefix(entry, p));
            if !injected {
                *dst = entry;
                dst = dst.add(1);
            }
            src = src.add(1);
        }
        *dst = null_mut();
    }
}

fn receiver_child(read_fd: i32, write_fd: i32) -> ! {
    sys::close(write_fd);
    let read_fd = sanitize_clone(read_fd, true);
    if read_fd < 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }
    if read_fd != libc::STDIN_FILENO {
        let _ = sys::dup2(read_fd, libc::STDIN_FILENO);
        sys::close(read_fd);
    }
    if state::CLOSE_FDS_ON_RECEIVER.load(Ordering::Relaxed) {
        let _ = sys::close_range_from(libc::STDERR_FILENO + 1);
    }
    strip_loader_injection_env();

    let path = state::meta().process_path.as_slice();
    if path.is_empty() || path[path.len() - 1] != 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }

    let argv = [path.as_ptr() as *const c_char, null_mut()];
    unsafe {
        libc::execv(path.as_ptr() as *const c_char, argv.as_ptr());
    }
    sys::exit_process(EXIT_CODE_FAILURE);
}

fn collector_child(read_fd: i32, write_fd: i32, event: CrashEvent) -> ! {
    sys::close(read_fd);
    let write_fd = sanitize_clone(write_fd, false);
    if write_fd < 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }
    ignore_sigpipe();

    let _ = emit_crash_report(write_fd, event, true);
    sys::exit_process(0);
}

fn emit_crash_report(write_fd: i32, event: CrashEvent, close_when_done: bool) -> bool {
    let mut frames = [0usize; config::BACKTRACE_LEVELS_MAX];
    let max_frames = state::MAX_FRAMES
        .load(Ordering::Relaxed)
        .min(config::BACKTRACE_LEVELS_MAX);
    let caps = capabilities::get();
    let can_walk = caps.contains(capabilities::PROC_VM_READV);
    let n = backtrace::backtrace_from_ucontext(
        &mut frames[..max_frames],
        event.ucontext,
        event.pid,
        can_walk,
    );
    let stackwalk_method = if n == 0 {
        "none"
    } else if can_walk {
        "fp_pvr"
    } else {
        "seed_only"
    };

    let meta = state::meta();
    let runtime_id = if meta.runtime_id.is_empty() {
        "00000000-0000-0000-0000-000000000000"
    } else {
        meta.runtime_id.as_str()
    };
    let report = Report {
        config_json: meta.config_json.as_str(),
        library_name: meta.library_name.as_str(),
        library_version: meta.library_version.as_str(),
        family: meta.family.as_str(),
        default_service: meta.default_service.as_str(),
        service: meta.service.as_str(),
        env: meta.env.as_str(),
        app_version: meta.app_version.as_str(),
        runtime_id,
        platform: meta.platform.as_str(),
        stackwalk_method,
        capabilities: caps,
        degradations: capabilities::degradations(),
    };
    let context = event.context(&frames[..n]);

    let mut sink = FdSink::new(write_fd);
    let emitted = super::emit_report(&mut sink, &report, &context);
    if close_when_done {
        sys::close(write_fd);
    }
    emitted
}

fn reap_or_kill(pid: i32, timeout_ms: i64, kill_process: bool) -> Option<i32> {
    match sys::reap_child(
        pid,
        timeout_ms,
        REAP_WAIT_INTERVAL_MS,
        kill_process,
        REAP_KILL_TIMEOUT_MS,
    ) {
        sys::ChildReap::Reaped(status) => Some(status),
        sys::ChildReap::WaitFailed(_) => {
            crash_debug(b"waitpid failed", -1);
            None
        }
        sys::ChildReap::NoChild | sys::ChildReap::TimedOut => None,
    }
}

fn exited_with(status: i32, code: i32) -> bool {
    libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == code
}

fn collect_crash(
    sig: i32,
    si_code: i32,
    has_info: bool,
    si_addr: usize,
    ucontext: *mut c_void,
    pid: i32,
    tid: i32,
) {
    let report_fd = state::REPORT_FD.load(Ordering::Relaxed);
    let caps = capabilities::get();
    let event = CrashEvent {
        sig,
        si_code,
        has_info,
        si_addr,
        pid,
        tid,
        ucontext,
    };

    let direct_report = |reason: capabilities::Degradations| {
        capabilities::note_degraded(reason);
        if caps.contains(capabilities::REPORT_FD_OK) {
            capabilities::note_degraded(capabilities::DEGRADED_REPORT_TO_FD);
            let _ = emit_crash_report(report_fd, event, false);
        }
    };

    if !caps.contains(capabilities::FORK_OK) {
        crash_debug(b"fork unavailable", sig);
        direct_report(capabilities::DEGRADED_NO_FORK);
        return;
    }
    if !caps.contains(capabilities::RECEIVER_OK) {
        crash_debug(b"receiver unavailable", sig);
        direct_report(capabilities::DEGRADED_RECEIVER_UNAVAILABLE);
        return;
    }
    if !caps.contains(capabilities::PIPE_OK) {
        crash_debug(b"pipe unavailable", sig);
        direct_report(capabilities::DEGRADED_NO_PIPE);
        return;
    }

    let mut fds = [0i32; 2];
    if !sys::pipe(&mut fds) {
        crash_debug(b"pipe failed", sig);
        direct_report(capabilities::DEGRADED_PIPE_FAILED);
        return;
    }

    let read_fd = fds[0];
    let write_fd = fds[1];

    let receiver = unsafe { sys::fork_raw() };
    if receiver == 0 {
        receiver_child(read_fd, write_fd);
    }
    if receiver < 0 {
        crash_debug(b"receiver fork failed", sig);
        sys::close(read_fd);
        sys::close(write_fd);
        direct_report(capabilities::DEGRADED_FORK_FAILED);
        return;
    }

    let collector = unsafe { sys::fork_raw() };
    if collector == 0 {
        collector_child(read_fd, write_fd, event);
    }

    sys::close(read_fd);
    sys::close(write_fd);

    if collector > 0 {
        let _ = reap_or_kill(
            collector as i32,
            state::COLLECTOR_REAP_MS.load(Ordering::Relaxed) as i64,
            true,
        );
    } else {
        crash_debug(b"collector fork failed", sig);
        direct_report(capabilities::DEGRADED_FORK_FAILED);
    }

    let receiver_status = reap_or_kill(
        receiver as i32,
        state::RECEIVER_TIMEOUT_MS.load(Ordering::Relaxed) as i64,
        true,
    );
    if receiver_status.is_some_and(|status| exited_with(status, EXIT_CODE_FAILURE)) {
        crash_debug(b"receiver exec failed", sig);
        direct_report(capabilities::DEGRADED_RECEIVER_UNAVAILABLE);
    }
}

extern "C" fn crash_handler(sig: c_int, info: *mut libc::siginfo_t, ucontext: *mut c_void) {
    if !state::HANDLERS_ENABLED.load(Ordering::Acquire) {
        return;
    }

    let saved_errno = sys::errno();
    crash_debug(b"handler entered", sig);
    let disarmed_on_entry =
        state::DISARM_ON_ENTRY.load(Ordering::Relaxed) && reset_signals_to_default(&[sig]);

    let idx = sig_index(sig);
    let has_info = !info.is_null();
    let si_code = if has_info {
        unsafe { (*info).si_code }
    } else {
        0
    };
    let si_addr = if has_info {
        unsafe { siginfo_addr(info) }
    } else {
        0
    };

    // Identity of the crashing thread, resolved once and reused by the app-handler chain guard
    // and the crash collection below.
    let self_pid = sys::getpid();
    let tid = sys::gettid();

    // The installed target is immutable while the handler runs, so resolve it once and reuse it for
    // both the app-first chain and the final chaining decision below.
    let target = match idx {
        Some(i) => effective_target(i),
        None => Target {
            fn_ptr: core::ptr::null_mut(),
            flags: 0,
        },
    };

    let force_on_top = state::FORCE_ON_TOP.load(Ordering::Relaxed);
    if let Some(i) = idx {
        let app_is_real = app_handler_is_real(target.fn_ptr);
        if should_run_app_first(force_on_top, app_is_real) {
            let stack_marker = 0u8;
            let stack_pos = (&stack_marker as *const u8) as usize;
            if enter_app_chain(tid, stack_pos) {
                sys::set_errno(saved_errno);
                // If the application handler recovers with siglongjmp, no code after this call
                // runs. Keep this path free of Drop-dependent state.
                unsafe { invoke_handler(&target, sig, info, ucontext) };

                let handler_after = live_handler_for_recovery(sig).unwrap_or(target.fn_ptr);
                leave_app_chain(tid, stack_pos);
                if app_recovered(handler_after) {
                    let pc = backtrace::instruction_pointer(ucontext);
                    if app_return_repeated_fault(i, pc, si_addr) {
                        crash_debug(b"app handler returned without recovery", sig);
                    } else {
                        if disarmed_on_entry {
                            restore_our_handler(sig);
                        }
                        sys::set_errno(saved_errno);
                        return;
                    }
                }
            } else {
                crash_debug(b"app handler recursion detected", sig);
            }
        }
    }

    let si_pid = if has_info {
        unsafe { siginfo_pid(info) }
    } else {
        0
    };
    let genuine_fault = is_genuine_fault(has_info, si_code, si_pid, self_pid);
    if genuine_fault && !COLLECTING.swap(true, Ordering::Relaxed) {
        collect_crash(sig, si_code, has_info, si_addr, ucontext, self_pid, tid);
    }

    sys::set_errno(saved_errno);

    let action = chain_action(disposition_of(target.fn_ptr), has_info, si_code);
    match action {
        ChainAction::RestoreDefaultAndRefault | ChainAction::RestoreDefaultAndReraise => {
            if !reset_signals_to_default(&[sig]) {
                sys::exit_process(EXIT_CODE_FAILURE);
            }
            unsafe {
                if let ChainAction::RestoreDefaultAndReraise = action {
                    unblock_signal(sig);
                    libc::raise(sig);
                    sys::exit_process(EXIT_CODE_FAILURE);
                }
            }
        }
        ChainAction::Resume => {
            if disarmed_on_entry {
                restore_our_handler(sig);
            }
        }
        ChainAction::InvokeApp => unsafe {
            if disarmed_on_entry && !genuine_fault {
                restore_our_handler(sig);
            }
            invoke_handler(&target, sig, info, ucontext);
        },
    }
}

fn live_handler_for_recovery(sig: c_int) -> Option<*mut c_void> {
    query_sigaction(sig).map(|cur| cur.sa_sigaction as *mut c_void)
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
unsafe fn siginfo_pid(info: *mut libc::siginfo_t) -> i32 {
    (*info).si_pid()
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
unsafe fn siginfo_pid(_info: *mut libc::siginfo_t) -> i32 {
    i32::MIN
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
unsafe fn siginfo_addr(info: *mut libc::siginfo_t) -> usize {
    (*info).si_addr() as usize
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
unsafe fn siginfo_addr(_info: *mut libc::siginfo_t) -> usize {
    0
}

fn query_sigaction(sig: c_int) -> Option<libc::sigaction> {
    let mut out: libc::sigaction = unsafe { core::mem::zeroed() };
    if unsafe { libc::sigaction(sig, null_mut(), &mut out) } == 0 {
        Some(out)
    } else {
        None
    }
}

fn is_our_handler(sig: c_int) -> bool {
    let Some(cur) = query_sigaction(sig) else {
        return false;
    };
    cur.sa_flags & libc::SA_SIGINFO != 0 && cur.sa_sigaction == crash_handler as *const () as usize
}

fn build_crash_sigaction() -> libc::sigaction {
    let mut sa: libc::sigaction = unsafe { core::mem::zeroed() };
    sa.sa_sigaction = crash_handler as *const () as usize;
    sa.sa_flags = libc::SA_SIGINFO;
    if state::USE_ALT_STACK.load(Ordering::Relaxed) {
        sa.sa_flags |= libc::SA_ONSTACK;
    }
    unsafe {
        libc::sigemptyset(&mut sa.sa_mask);
        if state::BLOCK_SIGNALS.load(Ordering::Relaxed) {
            for &blocked in &config::CRASH_SIGNALS {
                let _ = libc::sigaddset(&mut sa.sa_mask, blocked);
            }
        }
    }
    sa
}

fn restore_our_handler(sig: c_int) {
    let sa = build_crash_sigaction();
    unsafe {
        let _ = libc::sigaction(sig, &sa, null_mut());
    }
}

fn install_crash_handler(sig: c_int) {
    let Some(cur) = query_sigaction(sig) else {
        return;
    };
    if cur.sa_sigaction != libc::SIG_DFL {
        if app_handler_is_real(cur.sa_sigaction as *mut c_void) {
            if let Some(i) = sig_index(sig) {
                state::signal_slot(i).set_app_handler_present();
            }
            capabilities::note_degraded(capabilities::DEGRADED_APP_HANDLER_PRESENT);
            crash_debug(b"app handler present", sig);
        }
        return;
    }

    let sa = build_crash_sigaction();
    let mut old: libc::sigaction = unsafe { core::mem::zeroed() };
    if unsafe { libc::sigaction(sig, &sa, &mut old) } != 0 {
        return;
    }

    if let Some(i) = sig_index(sig) {
        state::signal_slot(i).store_original_handler(
            old.sa_sigaction as *mut c_void,
            old.sa_flags,
            &old.sa_mask,
        );
        state::signal_slot(i).set_owned(true);
    }
}

fn uninstall_crash_handler(sig: c_int) {
    if !is_our_handler(sig) {
        return;
    }
    let Some(i) = sig_index(sig) else {
        return;
    };

    let target = effective_target(i);
    let mut restore: libc::sigaction = unsafe { core::mem::zeroed() };
    restore.sa_sigaction = target.fn_ptr as usize;
    restore.sa_flags = target.flags;
    unsafe {
        state::signal_slot(i).load_original_mask(&mut restore.sa_mask);
        if libc::sigaction(sig, &restore, null_mut()) == 0 {
            state::signal_slot(i).set_owned(false);
        }
    }
}

fn install_all_handlers() {
    state::clear_signal_state();
    for &sig in &config::CRASH_SIGNALS {
        install_crash_handler(sig);
    }
}

fn uninstall_all_handlers() {
    for &sig in &config::CRASH_SIGNALS {
        uninstall_crash_handler(sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_chain_guard_distinguishes_recursion_from_unwind() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        APP_CHAIN_TID.store(0, Ordering::Relaxed);
        APP_CHAIN_STACK.store(0, Ordering::Relaxed);

        assert!(enter_app_chain(123, 1_000));
        assert!(!enter_app_chain(123, 900));
        assert!(!enter_app_chain(456, 1_100));
        assert!(enter_app_chain(123, 1_100));
        leave_app_chain(123, 1_100);
        assert_eq!(APP_CHAIN_TID.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn alt_stack_guard_failure_is_degraded_but_not_fatal() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        capabilities::publish(b"/definitely/missing-signal-safe-receiver\0", -1, false);
        assert!(install_alt_stack_with(|_, _| false, |_| true));
        assert!(capabilities::degradations()
            .contains(capabilities::DEGRADED_ALT_STACK_GUARD_UNAVAILABLE));
    }

    #[test]
    fn repeated_app_return_trips_on_second_same_fault() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");
        let idx = sig_index(libc::SIGSEGV).expect("SIGSEGV index");

        REPEAT_FAULT[idx].reset();

        assert!(!app_return_repeated_fault(idx, 0x1234, 0));
        assert!(app_return_repeated_fault(idx, 0x1234, 0));
        assert!(!app_return_repeated_fault(idx, 0x5678, 0));
    }

    #[cfg(not(feature = "collector"))]
    #[test]
    fn lifecycle_can_install_and_shutdown() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        let config = SignalSafeInitConfig {
            receiver_path: b"/bin/cat",
            ..SignalSafeInitConfig::default()
        };
        assert_eq!(init_result(&config), InitResult::Enabled);
        assert!(state::INSTALLED.load(Ordering::Acquire));
        assert_eq!(init_result(&config), InitResult::AlreadyInitialized);
        shutdown();
        assert!(!state::INSTALLED.load(Ordering::Acquire));
        assert_eq!(init_result(&config), InitResult::Enabled);
        assert!(state::INSTALLED.load(Ordering::Acquire));
        shutdown();
        assert!(!state::INSTALLED.load(Ordering::Acquire));
    }
}
