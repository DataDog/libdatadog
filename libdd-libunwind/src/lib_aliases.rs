// Macro to generate architecture-specific function aliases
// Usage: arch_aliases!(x86_64) generates aliases for x86_64 architecture
macro_rules! arch_aliases {
    ($arch:ident) => {
        ::paste::paste! {
            pub use {
                // Functions without "L" prefix (remote/generic functions)
                [<_U $arch _getcontext>] as unw_getcontext,
                [<_U $arch _strerror>] as unw_strerror,
                [<_U $arch _regname>] as unw_regname,
                
                // Functions with "L" prefix (local unwinding functions)
                [<_UL $arch _init_local>] as unw_init_local,
                [<_UL $arch _step>] as unw_step,
                [<_UL $arch _get_reg>] as unw_get_reg,
                [<_UL $arch _set_reg>] as unw_set_reg,
                [<_UL $arch _get_proc_name>] as unw_get_proc_name,
                [<_UL $arch _resume>] as unw_resume,
                [<_UL $arch _is_signal_frame>] as unw_is_signal_frame,
                [<_UL $arch _get_proc_info>] as unw_get_proc_info,
            };
        }
    };
}

// Create architecture-neutral aliases to standard unw_* names
// To add a new architecture, just add: arch_aliases!(new_arch);

#[cfg(target_arch = "x86_64")]
arch_aliases!(x86_64);

#[cfg(target_arch = "aarch64")]
arch_aliases!(aarch64);

// Add more architectures as needed:
// #[cfg(target_arch = "arm")]
// arch_aliases!(arm);
