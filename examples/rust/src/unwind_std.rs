use libc::{
    c_int, c_void, getpid, getuid, pid_t, sigaction, sigemptyset, sigset_t, size_t, syscall, uid_t,
    SYS_rt_tgsigqueueinfo, SA_SIGINFO, SIGRTMIN,
};
use std::{
    backtrace::Backtrace,
    mem::MaybeUninit,
    ptr::{addr_of, addr_of_mut, null_mut},
};

// please do not use this definition of siginfo_t in live code
#[repr(C)]
struct siginfo_t {
    si_signo: c_int,
    _si_errno: c_int,
    si_code: c_int,
    si_pid: pid_t,
    si_uid: uid_t,
    si_ptr: *mut c_void,
    _si_pad: [c_int; (128 / size_of::<c_int>()) - 3],
}

unsafe extern "C" fn handle_signal(_signo: c_int, _info: *mut siginfo_t, _ucontext: *const c_void) {
    // neither of these operations are signal safe
    let bt = Backtrace::force_capture();
    dbg!(bt);
}

fn main() {
    let sa_mask = unsafe {
        let mut sa_mask = MaybeUninit::<sigset_t>::uninit();
        sigemptyset(sa_mask.as_mut_ptr());
        sa_mask.assume_init()
    };

    let sa = sigaction {
        sa_sigaction: handle_signal as size_t,
        sa_mask,
        sa_flags: SA_SIGINFO,
        sa_restorer: None,
    };

    // please do not blindly copy this code for use in real world applications, it is meant to be
    // brief and functional, not strictly correct.
    unsafe {
        let _ = sigaction(SIGRTMIN(), addr_of!(sa), null_mut());

        let mut si: siginfo_t = MaybeUninit::zeroed().assume_init();
        si.si_signo = SIGRTMIN();
        si.si_code = -1; // SI_QUEUE
        si.si_pid = getpid();
        si.si_uid = getuid();

        let _ = syscall(
            SYS_rt_tgsigqueueinfo,
            getpid(),
            getpid(),
            SIGRTMIN(),
            addr_of_mut!(si),
        );
    }
}
