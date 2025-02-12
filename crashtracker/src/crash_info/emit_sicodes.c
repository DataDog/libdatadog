// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <signal.h>

//! Different OSes have different values for si_code constants
//! https://github.com/torvalds/linux/blob/master/include/uapi/asm-generic/siginfo.h
//! https://github.com/apple/darwin-xnu/blob/main/bsd/sys/signal.h
//! Ideally we would use libc::<CONSTANT_NAME> like we do for signum, but rust doesn't actually
//! export constants for si_code types.
//! As a workaround, link some C code which DOES have access to the types on the current platform
//! and use it to do the translation.

// MUST REMAIN IN SYNC WITH THE ENUM IN SIG_INFO.RS
enum SiCodes {
  SI_CODE_BUS_ADRALN,
  SI_CODE_BUS_ADRERR,
  SI_CODE_BUS_MCEERR_AO,
  SI_CODE_BUS_MCEERR_AR,
  SI_CODE_BUS_OBJERR,
  SI_CODE_ILL_BADSTK,
  SI_CODE_ILL_COPROC,
  SI_CODE_ILL_ILLADR,
  SI_CODE_ILL_ILLOPC,
  SI_CODE_ILL_ILLOPN,
  SI_CODE_ILL_ILLTRP,
  SI_CODE_ILL_PRVOPC,
  SI_CODE_ILL_PRVREG,
  SI_CODE_SEGV_ACCERR,
  SI_CODE_SEGV_BNDERR,
  SI_CODE_SEGV_MAPERR,
  SI_CODE_SEGV_PKUERR,
  SI_CODE_SI_ASYNCIO,
  SI_CODE_SI_KERNEL,
  SI_CODE_SI_MESGQ,
  SI_CODE_SI_QUEUE,
  SI_CODE_SI_SIGIO,
  SI_CODE_SI_TIMER,
  SI_CODE_SI_TKILL,
  SI_CODE_SI_USER,
  SI_CODE_SYS_SECCOMP,
  SI_CODE_UNKNOWN,
};

/// @brief  A best effort attempt to translate si_codes into the enum crashtracker understands.
/// @param signum
/// @param si_code
/// @return The enum value of the si_code, given signum. UNKNOWN if unable to translate.
int translate_si_code_impl(int signum, int si_code) {
  switch (si_code) {
  case SI_USER:
    return SI_CODE_SI_USER;
#ifdef SI_KERNEL
  case SI_KERNEL:
    return SI_CODE_SI_KERNEL;
#endif
#ifdef SI_TIMER
  case SI_TIMER:
    return SI_CODE_SI_TIMER;
#endif
  case SI_QUEUE:
    return SI_CODE_SI_QUEUE;
  case SI_MESGQ:
    return SI_CODE_SI_MESGQ;
  case SI_ASYNCIO:
    return SI_CODE_SI_ASYNCIO;
#ifdef SI_SIGIO
  case SI_SIGIO:
    return SI_CODE_SI_SIGIO;
#endif
#ifdef SI_TKILL
  case SI_TKILL:
    return SI_CODE_SI_TKILL;
#endif
  }

  switch (signum) {
  case SIGBUS:
    switch (si_code) {
    case BUS_ADRALN:
      return SI_CODE_BUS_ADRALN;
    case BUS_ADRERR:
      return SI_CODE_BUS_ADRERR;
#ifdef BUS_MCEERR_AO
    case BUS_MCEERR_AO:
      return SI_CODE_BUS_MCEERR_AO;
#endif
#ifdef BUS_MCEERR_AR
    case BUS_MCEERR_AR:
      return SI_CODE_BUS_MCEERR_AR;
#endif
    default:
      return SI_CODE_UNKNOWN;
    }

  case SIGSEGV:
    switch (si_code) {
    case SEGV_ACCERR:
      return SI_CODE_SEGV_ACCERR;
#ifdef SEGV_BNDERR
    case SEGV_BNDERR:
      return SI_CODE_SEGV_BNDERR;
#endif
    case SEGV_MAPERR:
      return SI_CODE_SEGV_MAPERR;
#ifdef SEGV_PKUERR
    case SEGV_PKUERR:
      return SI_CODE_SEGV_PKUERR;
#endif
    default:
      return SI_CODE_UNKNOWN;
    }

  default:
    return SI_CODE_UNKNOWN;
  }
}