// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, Ordering};

use super::config::{self, SignalSafeInitConfig};
use super::state::{self, sig_index, Stage};
use super::sys::{self, FdSink};
use super::{
    app_handler_is_real, app_recovered, chain_action, is_genuine_fault, should_run_app_first,
    ChainAction, CrashContext, Disposition, Report, SignalInfo,
};
use super::{backtrace, capabilities};
use crate::signal_owner::{self, SignalOwner};

const EXIT_CODE_FAILURE: i32 = 125;
const REAP_KILL_TIMEOUT_MS: i64 = 500;
const REAP_WAIT_INTERVAL_MS: i32 = 100;
const ALT_STACK_SIZE: usize = 64 * 1024;

struct AltStackStorage(UnsafeCell<[u8; ALT_STACK_SIZE]>);

unsafe impl Sync for AltStackStorage {}

static ALT_STACK: AltStackStorage = AltStackStorage(UnsafeCell::new([0; ALT_STACK_SIZE]));

unsafe extern "C" {
    static mut environ: *mut *mut c_char;
}

#[derive(Clone, Copy)]
struct Target {
    fn_ptr: *mut c_void,
    flags: i32,
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitResult {
    Enabled = 0,
    DisabledByConfig = 1,
    Failed = 2,
}

pub fn init(config: &SignalSafeInitConfig<'_>) -> bool {
    matches!(init_result(config), InitResult::Enabled)
}

pub fn init_result(config: &SignalSafeInitConfig<'_>) -> InitResult {
    if !state::begin_init() {
        return InitResult::Failed;
    }
    if !signal_owner::acquire(SignalOwner::SignalSafeCollector) {
        state::fail_init();
        return InitResult::Failed;
    }
    if !config::prepare(config) {
        signal_owner::release(SignalOwner::SignalSafeCollector);
        state::fail_init();
        return InitResult::Failed;
    }
    if !install_alt_stack_if_requested() {
        signal_owner::release(SignalOwner::SignalSafeCollector);
        state::fail_init();
        return InitResult::Failed;
    }
    install_all_handlers();
    state::set_stage(Stage::CrashtrackerInit);
    state::INSTALLED.store(true, Ordering::Release);
    state::finish_init();
    state::HANDLERS_ENABLED.store(true, Ordering::Release);
    InitResult::Enabled
}

pub fn init_from_env() -> bool {
    matches!(init_from_env_result(), InitResult::Enabled)
}

pub fn init_from_env_result() -> InitResult {
    if config::disabled_by_env() {
        return InitResult::DisabledByConfig;
    }
    if !state::begin_init() {
        return InitResult::Failed;
    }
    if !signal_owner::acquire(SignalOwner::SignalSafeCollector) {
        state::fail_init();
        return InitResult::Failed;
    }
    if !config::prepare_from_env() {
        signal_owner::release(SignalOwner::SignalSafeCollector);
        state::fail_init();
        return InitResult::Failed;
    }
    if !install_alt_stack_if_requested() {
        signal_owner::release(SignalOwner::SignalSafeCollector);
        state::fail_init();
        return InitResult::Failed;
    }
    install_all_handlers();
    state::set_stage(Stage::CrashtrackerInit);
    state::INSTALLED.store(true, Ordering::Release);
    state::finish_init();
    state::HANDLERS_ENABLED.store(true, Ordering::Release);
    InitResult::Enabled
}

pub fn bootstrap_complete() {
    if state::ONLY_BOOTSTRAP.load(Ordering::Relaxed) {
        shutdown();
    } else {
        state::set_stage(Stage::Application);
    }
}

pub fn shutdown() {
    state::set_stage(Stage::CrashtrackerUninstall);
    state::HANDLERS_ENABLED.store(false, Ordering::Release);
    uninstall_all_handlers();
    state::INSTALLED.store(false, Ordering::Release);
    signal_owner::release(SignalOwner::SignalSafeCollector);
}

fn effective_target(idx: usize) -> Target {
    Target {
        fn_ptr: state::ORIG_FN[idx].load(Ordering::Relaxed),
        flags: state::ORIG_FLAGS[idx].load(Ordering::Relaxed),
    }
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

fn crash_debug(msg: &[u8], sig: i32) {
    if !state::DEBUG_LOG.load(Ordering::Relaxed) {
        return;
    }
    let mut sink = FdSink::new(libc::STDERR_FILENO);
    let _ = super::Sink::put(&mut sink, b"dd-crashtracker[signal-safe]: ");
    let _ = super::Sink::put(&mut sink, msg);
    if sig >= 0 {
        let _ = super::Sink::put(&mut sink, b" ");
        let mut buf = [0u8; 12];
        let written = write_i32(sig, &mut buf);
        let _ = super::Sink::put(&mut sink, &buf[..written]);
    }
    let _ = super::Sink::put(&mut sink, b"\n");
}

fn write_i32(value: i32, out: &mut [u8; 12]) -> usize {
    let mut n = value as i64;
    let negative = n < 0;
    if negative {
        n = n.wrapping_neg();
    }

    let mut tmp = [0u8; 11];
    let mut len = 0usize;
    loop {
        tmp[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }

    let mut off = 0usize;
    if negative {
        out[0] = b'-';
        off = 1;
    }
    let mut i = 0usize;
    while i < len {
        out[off + i] = tmp[len - i - 1];
        i += 1;
    }
    off + len
}

fn sanitize_clone(mut keep_fd: i32) -> i32 {
    if (libc::STDIN_FILENO..=libc::STDERR_FILENO).contains(&keep_fd) {
        let relocated = sys::fcntl_dupfd(keep_fd, libc::STDERR_FILENO + 1);
        if relocated < 0 {
            return -1;
        }
        sys::close(keep_fd);
        keep_fd = relocated;
    }

    reset_handlers_to_default();

    let devnull = sys::open_readwrite(c"/dev/null".as_ptr().cast());
    if devnull >= 0 {
        let _ = sys::dup2(devnull, libc::STDIN_FILENO);
        let _ = sys::dup2(devnull, libc::STDOUT_FILENO);
        let _ = sys::dup2(devnull, libc::STDERR_FILENO);
        if devnull > libc::STDERR_FILENO {
            sys::close(devnull);
        }
    }
    keep_fd
}

fn reset_handlers_to_default() {
    let mut dfl: libc::sigaction = unsafe { core::mem::zeroed() };
    dfl.sa_sigaction = libc::SIG_DFL;
    unsafe {
        libc::sigemptyset(&mut dfl.sa_mask);
    }
    for &sig in &config::CRASH_SIGNALS {
        unsafe {
            let _ = libc::sigaction(sig, &dfl, null_mut());
        }
    }
}

fn reset_signal_to_default(sig: c_int) -> bool {
    let mut dfl: libc::sigaction = unsafe { core::mem::zeroed() };
    dfl.sa_sigaction = libc::SIG_DFL;
    unsafe {
        libc::sigemptyset(&mut dfl.sa_mask);
        libc::sigaction(sig, &dfl, null_mut()) == 0
    }
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

    let stack = libc::stack_t {
        ss_sp: ALT_STACK.0.get().cast(),
        ss_flags: 0,
        ss_size: ALT_STACK_SIZE,
    };
    unsafe { libc::sigaltstack(&stack, null_mut()) == 0 }
}

fn strip_loader_injection_env() {
    let env = unsafe { environ };
    if env.is_null() {
        return;
    }
    const PREFIXES: [&[u8]; 2] = [b"LD_PRELOAD=", b"LD_AUDIT="];
    unsafe {
        let mut src = env;
        let mut dst = env;
        while !(*src).is_null() {
            let entry = *src;
            let injected = PREFIXES.iter().any(|p| cstr_starts_with(entry, p));
            if !injected {
                *dst = entry;
                dst = dst.add(1);
            }
            src = src.add(1);
        }
        *dst = null_mut();
    }
}

unsafe fn cstr_starts_with(s: *const c_char, prefix: &[u8]) -> bool {
    let mut i = 0usize;
    while i < prefix.len() {
        let c = *s.add(i);
        if c == 0 || c as u8 != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn receiver_child(read_fd: i32, write_fd: i32) -> ! {
    sys::close(write_fd);
    let read_fd = sanitize_clone(read_fd);
    if read_fd < 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }
    if read_fd != libc::STDIN_FILENO {
        let _ = sys::dup2(read_fd, libc::STDIN_FILENO);
        sys::close(read_fd);
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

#[allow(clippy::too_many_arguments)]
fn collector_child(
    read_fd: i32,
    write_fd: i32,
    sig: i32,
    si_code: i32,
    has_info: bool,
    si_addr: usize,
    pid: i32,
    tid: i32,
    ucontext: *mut c_void,
) -> ! {
    sys::close(read_fd);
    let write_fd = sanitize_clone(write_fd);
    if write_fd < 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }

    let _ = emit_crash_report(
        write_fd, sig, si_code, has_info, si_addr, pid, tid, ucontext, true,
    );
    sys::exit_process(0);
}

#[allow(clippy::too_many_arguments)]
fn emit_crash_report(
    write_fd: i32,
    sig: i32,
    si_code: i32,
    has_info: bool,
    si_addr: usize,
    pid: i32,
    tid: i32,
    ucontext: *mut c_void,
    close_when_done: bool,
) -> bool {
    let mut frames = [0usize; config::BACKTRACE_LEVELS_MAX];
    let max_frames = state::MAX_FRAMES
        .load(Ordering::Relaxed)
        .min(config::BACKTRACE_LEVELS_MAX);
    let can_walk = capabilities::has(capabilities::PROC_VM_READV);
    let n = backtrace::backtrace_from_ucontext(&mut frames[..max_frames], ucontext, pid, can_walk);
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
        stage_name: state::current_stage_name(),
        stackwalk_method,
        degradation_bits: capabilities::degradations(),
    };
    let context = CrashContext {
        signal: SignalInfo::new(sig, si_code, si_addr, has_info),
        pid,
        tid,
        frames: &frames[..n],
    };

    let mut sink = FdSink::new(write_fd);
    let emitted = super::emit_report(&mut sink, &report, &context);
    if close_when_done {
        sys::close(write_fd);
    }
    emitted
}

fn reap_or_kill(pid: i32, timeout_ms: i64, kill_process: bool) {
    let start = sys::monotonic_nanos();
    loop {
        let waited = sys::waitpid_nohang(pid);
        if waited == pid {
            return;
        }
        if waited < 0 {
            if waited != -libc::ECHILD {
                crash_debug(b"waitpid failed", -1);
            }
            return;
        }

        sys::poll_sleep_ms(REAP_WAIT_INTERVAL_MS);
        let elapsed_ms = (sys::monotonic_nanos() - start) / 1_000_000;
        if elapsed_ms >= timeout_ms {
            if kill_process {
                let _ = sys::kill(pid, libc::SIGKILL);
                reap_or_kill(pid, REAP_KILL_TIMEOUT_MS, false);
            }
            return;
        }
    }
}

fn collect_crash(sig: i32, si_code: i32, has_info: bool, si_addr: usize, ucontext: *mut c_void) {
    let pid = sys::getpid();
    let tid = sys::gettid();
    let report_fd = state::REPORT_FD.load(Ordering::Relaxed);

    let direct_report = |reason: u32| {
        capabilities::note_degraded(reason | capabilities::DEGRADED_REPORT_TO_FD);
        if report_fd >= 0 {
            let _ = emit_crash_report(
                report_fd, sig, si_code, has_info, si_addr, pid, tid, ucontext, false,
            );
        }
    };

    if !capabilities::has(capabilities::FORK_OK) {
        crash_debug(b"fork unavailable", sig);
        direct_report(capabilities::DEGRADED_NO_FORK);
        return;
    }
    if !capabilities::has(capabilities::RECEIVER_OK) {
        crash_debug(b"receiver unavailable", sig);
        direct_report(capabilities::DEGRADED_RECEIVER_UNAVAILABLE);
        return;
    }
    if !capabilities::has(capabilities::PIPE_OK) {
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

    let collector: isize;
    if receiver > 0 {
        collector = unsafe { sys::fork_raw() };
        if collector == 0 {
            collector_child(
                read_fd, write_fd, sig, si_code, has_info, si_addr, pid, tid, ucontext,
            );
        }
    } else {
        crash_debug(b"receiver fork failed", sig);
        sys::close(read_fd);
        sys::close(write_fd);
        direct_report(capabilities::DEGRADED_FORK_FAILED);
        return;
    }

    sys::close(read_fd);
    sys::close(write_fd);

    if collector > 0 {
        reap_or_kill(
            collector as i32,
            state::COLLECTOR_REAP_MS.load(Ordering::Relaxed) as i64,
            true,
        );
    } else if receiver > 0 {
        crash_debug(b"collector fork failed", sig);
        direct_report(capabilities::DEGRADED_FORK_FAILED);
    }
    if receiver > 0 {
        reap_or_kill(
            receiver as i32,
            state::RECEIVER_TIMEOUT_MS.load(Ordering::Relaxed) as i64,
            true,
        );
    }
}

extern "C" fn crash_handler(sig: c_int, info: *mut libc::siginfo_t, ucontext: *mut c_void) {
    if !state::HANDLERS_ENABLED.load(Ordering::Acquire) {
        return;
    }

    let saved_errno = sys::errno();
    crash_debug(b"handler entered", sig);
    if state::DISARM_ON_ENTRY.load(Ordering::Relaxed) {
        let _ = reset_signal_to_default(sig);
    }

    let idx = sig_index(sig);
    let has_info = !info.is_null();
    let si_code = if has_info {
        unsafe { (*info).si_code }
    } else {
        0
    };

    let force_on_top = state::FORCE_ON_TOP.load(Ordering::Relaxed);
    if let Some(i) = idx {
        let target = effective_target(i);
        let app_is_real = app_handler_is_real(target.fn_ptr);
        if should_run_app_first(force_on_top, app_is_real) {
            static IN_APP_CHAIN: AtomicBool = AtomicBool::new(false);
            if !IN_APP_CHAIN.swap(true, Ordering::Relaxed) {
                sys::set_errno(saved_errno);
                // If the application handler recovers with siglongjmp, no code after this call
                // runs. Keep this path free of Drop-dependent state.
                unsafe { invoke_handler(&target, sig, info, ucontext) };

                let handler_after = live_handler_for_recovery(sig).unwrap_or(target.fn_ptr);
                IN_APP_CHAIN.store(false, Ordering::Relaxed);
                if app_recovered(handler_after) {
                    sys::set_errno(saved_errno);
                    return;
                }
            }
        }
    }

    let self_pid = sys::getpid();
    let si_pid = if has_info {
        unsafe { siginfo_pid(info) }
    } else {
        0
    };
    if is_genuine_fault(has_info, si_code, si_pid, self_pid) {
        static COLLECTING: AtomicBool = AtomicBool::new(false);
        if !COLLECTING.swap(true, Ordering::Relaxed) {
            let si_addr = if has_info {
                unsafe { siginfo_addr(info) }
            } else {
                0
            };
            collect_crash(sig, si_code, has_info, si_addr, ucontext);
        }
    }

    sys::set_errno(saved_errno);

    let target = match idx {
        Some(i) => effective_target(i),
        None => Target {
            fn_ptr: core::ptr::null_mut(),
            flags: 0,
        },
    };
    let action = chain_action(disposition_of_target(target.fn_ptr), has_info, si_code);
    match action {
        ChainAction::RestoreDefaultAndRefault | ChainAction::RestoreDefaultAndReraise => {
            if !reset_signal_to_default(sig) {
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
        ChainAction::Resume => {}
        ChainAction::InvokeApp => unsafe {
            invoke_handler(&target, sig, info, ucontext);
        },
    }
}

fn disposition_of_target(handler: *mut c_void) -> Disposition {
    super::disposition_of(handler)
}

fn live_handler_for_recovery(sig: c_int) -> Option<*mut c_void> {
    let mut cur: libc::sigaction = unsafe { core::mem::zeroed() };
    if query_sigaction(sig, &mut cur) {
        Some(cur.sa_sigaction as *mut c_void)
    } else {
        None
    }
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

fn query_sigaction(sig: c_int, out: *mut libc::sigaction) -> bool {
    unsafe { libc::sigaction(sig, null_mut(), out) == 0 }
}

fn is_default_handler(sig: c_int) -> bool {
    let mut cur: libc::sigaction = unsafe { core::mem::zeroed() };
    if !query_sigaction(sig, &mut cur) {
        return false;
    }
    cur.sa_sigaction == libc::SIG_DFL
}

fn is_our_handler(sig: c_int) -> bool {
    let mut cur: libc::sigaction = unsafe { core::mem::zeroed() };
    if !query_sigaction(sig, &mut cur) {
        return false;
    }
    cur.sa_flags & libc::SA_SIGINFO != 0 && cur.sa_sigaction == crash_handler as *const () as usize
}

fn install_crash_handler(sig: c_int) {
    if !is_default_handler(sig) {
        return;
    }

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

    let mut old: libc::sigaction = unsafe { core::mem::zeroed() };
    if unsafe { libc::sigaction(sig, &sa, &mut old) } != 0 {
        return;
    }

    if let Some(i) = sig_index(sig) {
        state::ORIG_FN[i].store(old.sa_sigaction as *mut c_void, Ordering::Relaxed);
        state::ORIG_FLAGS[i].store(old.sa_flags, Ordering::Relaxed);
        state::OWN_SIGNAL[i].store(true, Ordering::Relaxed);
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
        libc::sigemptyset(&mut restore.sa_mask);
        if libc::sigaction(sig, &restore, null_mut()) == 0 {
            state::OWN_SIGNAL[i].store(false, Ordering::Relaxed);
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
    fn integer_debug_writer_handles_sign() {
        let mut buf = [0u8; 12];
        let n = write_i32(-123, &mut buf);
        assert_eq!(&buf[..n], b"-123");
        let n = write_i32(42, &mut buf);
        assert_eq!(&buf[..n], b"42");
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
        assert!(init(&config));
        assert!(state::INSTALLED.load(Ordering::Acquire));
        shutdown();
        assert!(!state::INSTALLED.load(Ordering::Acquire));
    }
}
