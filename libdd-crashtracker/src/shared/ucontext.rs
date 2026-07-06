// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UcontextRegisters {
    pub ip: usize,
    pub sp: usize,
    pub fp: usize,
    pub link: usize,
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_arch = "x86_64"
))]
pub fn ucontext_registers(uc: &libc::ucontext_t) -> Option<UcontextRegisters> {
    Some(UcontextRegisters {
        ip: uc.uc_mcontext.gregs[libc::REG_RIP as usize] as usize,
        sp: uc.uc_mcontext.gregs[libc::REG_RSP as usize] as usize,
        fp: uc.uc_mcontext.gregs[libc::REG_RBP as usize] as usize,
        link: 0,
    })
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_arch = "aarch64"
))]
pub fn ucontext_registers(uc: &libc::ucontext_t) -> Option<UcontextRegisters> {
    Some(UcontextRegisters {
        ip: uc.uc_mcontext.pc as usize,
        sp: uc.uc_mcontext.sp as usize,
        fp: uc.uc_mcontext.regs[29] as usize,
        link: uc.uc_mcontext.regs[30] as usize,
    })
}

#[cfg(all(target_vendor = "apple", target_arch = "x86_64"))]
pub fn ucontext_registers(uc: &libc::ucontext_t) -> Option<UcontextRegisters> {
    let mcontext = uc.uc_mcontext;
    if mcontext.is_null() {
        return None;
    }
    let ss = unsafe { &(*mcontext).__ss };
    Some(UcontextRegisters {
        ip: ss.__rip as usize,
        sp: ss.__rsp as usize,
        fp: ss.__rbp as usize,
        link: 0,
    })
}

#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
pub fn ucontext_registers(uc: &libc::ucontext_t) -> Option<UcontextRegisters> {
    let mcontext = uc.uc_mcontext;
    if mcontext.is_null() {
        return None;
    }
    let ss = unsafe { &(*mcontext).__ss };
    Some(UcontextRegisters {
        ip: ss.__pc as usize,
        sp: ss.__sp as usize,
        fp: ss.__fp as usize,
        link: ss.__lr as usize,
    })
}

#[cfg(not(any(
    all(
        any(target_os = "linux", target_os = "android"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(
        target_vendor = "apple",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
pub fn ucontext_registers(_uc: &libc::ucontext_t) -> Option<UcontextRegisters> {
    None
}
