// Forward of the symbolizer capis from blazesym.

use std::alloc::alloc;
use std::alloc::dealloc;
use std::alloc::Layout;
use std::ffi::CStr;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::mem;
use std::os::raw::c_char;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;

use blazesym::symbolize::CodeInfo;
use blazesym::symbolize::Elf;
use blazesym::symbolize::GsymData;
use blazesym::symbolize::GsymFile;
use blazesym::symbolize::InlinedFn;
use blazesym::symbolize::Input;
use blazesym::symbolize::Kernel;
use blazesym::symbolize::Process;
use blazesym::symbolize::Source;
use blazesym::symbolize::Sym;
use blazesym::symbolize::Symbolized;
use blazesym::symbolize::Symbolizer;
pub use blazesym::Addr;

use std::ptr::NonNull;
use std::slice;

/// "Safely" create a slice from a user provided array.
pub unsafe fn slice_from_user_array<'t, T>(items: *const T, num_items: usize) -> &'t [T] {
    let items = if items.is_null() {
        // `slice::from_raw_parts` requires a properly aligned non-NULL pointer.
        // Craft one.
        NonNull::dangling().as_ptr()
    } else {
        items
    };
    unsafe { slice::from_raw_parts(items, num_items) }
}

/// The parameters to load symbols and debug information from an ELF.
///
/// Describes the path and address of an ELF file loaded in a
/// process.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_symbolize_src_elf {
    /// The path to the ELF file.
    ///
    /// The referenced file may be an executable or shared object. For example,
    /// passing "/bin/sh" will load symbols and debug information from `sh` and
    /// passing "/lib/libc.so.xxx" will load symbols and debug information from
    /// libc.
    pub path: *const c_char,
    /// Whether or not to consult debug symbols to satisfy the request
    /// (if present).
    pub debug_syms: bool,
}

impl From<&blaze_symbolize_src_elf> for Elf {
    fn from(elf: &blaze_symbolize_src_elf) -> Self {
        let blaze_symbolize_src_elf { path, debug_syms } = elf;
        Self {
            path: unsafe { from_cstr(*path) },
            debug_syms: *debug_syms,
            _non_exhaustive: (),
        }
    }
}

/// The parameters to load symbols and debug information from a kernel.
///
/// Use a kernel image and a snapshot of its kallsyms as a source of symbols and
/// debug information.
#[repr(C)]
#[derive(Debug, PartialEq)]
pub struct blaze_symbolize_src_kernel {
    /// The path of a copy of kallsyms.
    ///
    /// It can be `"/proc/kallsyms"` for the running kernel on the
    /// device.  However, you can make copies for later.  In that situation,
    /// you should give the path of a copy.
    /// Passing a `NULL`, by default, will result in `"/proc/kallsyms"`.
    pub kallsyms: *const c_char,
    /// The path of a kernel image.
    ///
    /// The path of a kernel image should be, for instance,
    /// `"/boot/vmlinux-xxxx"`.  For a `NULL` value, it will locate the
    /// kernel image of the running kernel in `"/boot/"` or
    /// `"/usr/lib/debug/boot/"`.
    pub kernel_image: *const c_char,
    /// Whether or not to consult debug symbols from `kernel_image`
    /// to satisfy the request (if present).
    pub debug_syms: bool,
}

impl From<&blaze_symbolize_src_kernel> for Kernel {
    fn from(kernel: &blaze_symbolize_src_kernel) -> Self {
        let blaze_symbolize_src_kernel {
            kallsyms,
            kernel_image,
            debug_syms,
        } = kernel;
        Self {
            kallsyms: (!kallsyms.is_null()).then(|| unsafe { from_cstr(*kallsyms) }),
            kernel_image: (!kernel_image.is_null()).then(|| unsafe { from_cstr(*kernel_image) }),
            debug_syms: *debug_syms,
            _non_exhaustive: (),
        }
    }
}

/// The parameters to load symbols and debug information from a process.
///
/// Load all ELF files in a process as the sources of symbols and debug
/// information.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_symbolize_src_process {
    /// It is the PID of a process to symbolize.
    ///
    /// blazesym will parse `/proc/<pid>/maps` and load all the object
    /// files.
    pub pid: u32,
    /// Whether or not to consult debug symbols to satisfy the request
    /// (if present).
    pub debug_syms: bool,
}

impl From<&blaze_symbolize_src_process> for Process {
    fn from(process: &blaze_symbolize_src_process) -> Self {
        let blaze_symbolize_src_process { pid, debug_syms } = process;
        Self {
            pid: (*pid).into(),
            debug_syms: *debug_syms,
            _non_exhaustive: (),
        }
    }
}

/// The parameters to load symbols and debug information from "raw" Gsym data.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_symbolize_src_gsym_data {
    /// The Gsym data.
    pub data: *const u8,
    /// The size of the Gsym data.
    pub data_len: usize,
}

impl From<&blaze_symbolize_src_gsym_data> for GsymData<'_> {
    fn from(gsym: &blaze_symbolize_src_gsym_data) -> Self {
        let blaze_symbolize_src_gsym_data { data, data_len } = gsym;
        Self {
            data: unsafe { slice_from_user_array(*data, *data_len) },
            _non_exhaustive: (),
        }
    }
}

/// The parameters to load symbols and debug information from a Gsym file.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_symbolize_src_gsym_file {
    /// The path to a gsym file.
    pub path: *const c_char,
}

impl From<&blaze_symbolize_src_gsym_file> for GsymFile {
    fn from(gsym: &blaze_symbolize_src_gsym_file) -> Self {
        let blaze_symbolize_src_gsym_file { path } = gsym;
        Self {
            path: unsafe { from_cstr(*path) },
            _non_exhaustive: (),
        }
    }
}

/// C ABI compatible version of [`blazesym::symbolize::Symbolizer`].
///
/// It is returned by [`blaze_symbolizer_new`] and should be free by
/// [`blaze_symbolizer_free`].
pub type blaze_symbolizer = Symbolizer;

/// Source code location information for a symbol or inlined function.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_symbolize_code_info {
    /// The directory in which the source file resides.
    ///
    /// This attribute is optional and may be NULL.
    pub dir: *const c_char,
    /// The file that defines the symbol.
    ///
    /// This attribute is optional and may be NULL.
    pub file: *const c_char,
    /// The line number on which the symbol is located in the source
    /// code.
    pub line: u32,
    /// The column number of the symbolized instruction in the source
    /// code.
    pub column: u16,
}

/// Data about an inlined function call.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_symbolize_inlined_fn {
    /// The symbol name of the inlined function.
    pub name: *const c_char,
    /// Source code location information for the inlined function.
    pub code_info: blaze_symbolize_code_info,
}

/// The result of symbolization of an address.
///
/// A `blaze_sym` is the information of a symbol found for an
/// address.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_sym {
    /// The symbol name is where the given address should belong to.
    ///
    /// If an address could not be symbolized, this member will be NULL.
    pub name: *const c_char,
    /// The address at which the symbol is located (i.e., its "start").
    ///
    /// This is the "normalized" address of the symbol, as present in
    /// the file (and reported by tools such as `readelf(1)`,
    /// `llvm-gsymutil`, or similar).
    pub addr: Addr,
    /// The byte offset of the address that got symbolized from the
    /// start of the symbol (i.e., from `addr`).
    ///
    /// E.g., when normalizing address 0x1337 of a function that starts at
    /// 0x1330, the offset will be set to 0x07 (and `addr` will be 0x1330). This
    /// member is especially useful in contexts when input addresses are not
    /// already normalized, such as when normalizing an address in a process
    /// context (which may have been relocated and/or have layout randomizations
    /// applied).
    pub offset: usize,
    /// Source code location information for the symbol.
    pub code_info: blaze_symbolize_code_info,
    /// The number of symbolized inlined function calls present.
    pub inlined_cnt: usize,
    /// An array of `inlined_cnt` symbolized inlined function calls.
    pub inlined: *const blaze_symbolize_inlined_fn,
}

/// `blaze_result` is the result of symbolization for C API.
///
/// Instances of [`blaze_result`] are returned by any of the `blaze_symbolize_*`
/// variants. They should be freed by calling [`blaze_result_free`].
#[repr(C)]
#[derive(Debug)]
pub struct blaze_result {
    /// The number of symbols being reported.
    pub cnt: usize,
    /// The symbols corresponding to input addresses.
    ///
    /// Symbolization happens based on the ordering of (input) addresses.
    /// Therefore, every input address has an associated symbol.
    pub syms: [blaze_sym; 0],
}

/// Create a `PathBuf` from a pointer of C string
///
/// # Safety
/// The provided `cstr` should be terminated with a NUL byte.
unsafe fn from_cstr(cstr: *const c_char) -> PathBuf {
    Path::new(OsStr::from_bytes(
        unsafe { CStr::from_ptr(cstr) }.to_bytes(),
    ))
    .to_path_buf()
}

/// Options for configuring `blaze_symbolizer` objects.
#[repr(C)]
#[derive(Debug)]
pub struct blaze_symbolizer_opts {
    /// Whether to attempt to gather source code location information.
    ///
    /// This setting implies `debug_syms` (and forces it to `true`).
    pub code_info: bool,
    /// Whether to report inlined functions as part of symbolization.
    pub inlined_fns: bool,
    /// Whether or not to transparently demangle symbols.
    ///
    /// Demangling happens on a best-effort basis. Currently supported
    /// languages are Rust and C++ and the flag will have no effect if
    /// the underlying language does not mangle symbols (such as C).
    pub demangle: bool,
}

/// Create an instance of a symbolizer.
#[no_mangle]
pub extern "C" fn blaze_symbolizer_new() -> *mut blaze_symbolizer {
    let symbolizer = Symbolizer::new();
    let symbolizer_box = Box::new(symbolizer);
    Box::into_raw(symbolizer_box)
}

/// Create an instance of a symbolizer with configurable options.
///
/// # Safety
/// `opts` needs to be a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolizer_new_opts(
    opts: *const blaze_symbolizer_opts,
) -> *mut blaze_symbolizer {
    // SAFETY: The caller ensures that the pointer is valid.
    let opts = unsafe { &*opts };
    let blaze_symbolizer_opts {
        code_info,
        inlined_fns,
        demangle,
    } = opts;

    let symbolizer = Symbolizer::builder()
        .enable_code_info(*code_info)
        .enable_inlined_fns(*inlined_fns)
        .enable_demangling(*demangle)
        .build();
    let symbolizer_box = Box::new(symbolizer);
    Box::into_raw(symbolizer_box)
}

/// Free an instance of blazesym a symbolizer for C API.
///
/// # Safety
///
/// The pointer must have been returned by [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`].
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolizer_free(symbolizer: *mut blaze_symbolizer) {
    if !symbolizer.is_null() {
        drop(unsafe { Box::from_raw(symbolizer) });
    }
}

fn code_info_strtab_size(code_info: &Option<CodeInfo>) -> usize {
    code_info
        .as_ref()
        .and_then(|info| info.dir.as_ref().map(|d| d.as_os_str().len() + 1))
        .unwrap_or(0)
        + code_info
            .as_ref()
            .map(|info| info.file.len() + 1)
            .unwrap_or(0)
}

fn inlined_fn_strtab_size(inlined_fn: &InlinedFn) -> usize {
    inlined_fn.name.len() + 1 + code_info_strtab_size(&inlined_fn.code_info)
}

fn sym_strtab_size(sym: &Sym) -> usize {
    sym.name.len()
        + 1
        + code_info_strtab_size(&sym.code_info)
        + sym
            .inlined
            .iter()
            .map(inlined_fn_strtab_size)
            .sum::<usize>()
}

fn convert_code_info(
    code_info_in: &Option<CodeInfo>,
    code_info_out: &mut blaze_symbolize_code_info,
    mut make_cstr: impl FnMut(&OsStr) -> *mut c_char,
) {
    code_info_out.dir = code_info_in
        .as_ref()
        .and_then(|info| info.dir.as_ref().map(|d| make_cstr(d.as_os_str())))
        .unwrap_or_else(ptr::null_mut);
    code_info_out.file = code_info_in
        .as_ref()
        .map(|info| make_cstr(&info.file))
        .unwrap_or_else(ptr::null_mut);
    code_info_out.line = code_info_in
        .as_ref()
        .and_then(|info| info.line)
        .unwrap_or(0);
    code_info_out.column = code_info_in
        .as_ref()
        .and_then(|info| info.column)
        .unwrap_or(0);
}

/// Convert [`Sym`] objects to [`blaze_result`] ones.
///
/// The returned pointer should be released using [`blaze_result_free`] once
/// usage concluded.
fn convert_symbolizedresults_to_c(results: Vec<Symbolized>) -> *const blaze_result {
    // Allocate a buffer to contain a blaze_result, all
    // blaze_sym, and C strings of symbol and path.
    let (strtab_size, inlined_fn_cnt) = results.iter().fold((0, 0), |acc, sym| match sym {
        Symbolized::Sym(sym) => (acc.0 + sym_strtab_size(sym), acc.1 + sym.inlined.len()),
        Symbolized::Unknown => acc,
    });

    let buf_size = strtab_size
        + mem::size_of::<blaze_result>()
        + mem::size_of::<blaze_sym>() * results.len()
        + mem::size_of::<blaze_symbolize_inlined_fn>() * inlined_fn_cnt;
    let raw_buf_with_sz =
        unsafe { alloc(Layout::from_size_align(buf_size + mem::size_of::<u64>(), 8).unwrap()) };
    if raw_buf_with_sz.is_null() {
        return ptr::null();
    }

    // prepend an u64 to keep the size of the buffer.
    unsafe { *(raw_buf_with_sz as *mut u64) = buf_size as u64 };

    let raw_buf = unsafe { raw_buf_with_sz.add(mem::size_of::<u64>()) };

    let result_ptr = raw_buf as *mut blaze_result;
    let mut syms_last = unsafe { &mut (*result_ptr).syms as *mut blaze_sym };
    let mut inlined_last = unsafe {
        raw_buf.add(mem::size_of::<blaze_result>() + mem::size_of::<blaze_sym>() * results.len())
    } as *mut blaze_symbolize_inlined_fn;
    let mut cstr_last = unsafe {
        raw_buf.add(
            mem::size_of::<blaze_result>()
                + mem::size_of::<blaze_sym>() * results.len()
                + mem::size_of::<blaze_symbolize_inlined_fn>() * inlined_fn_cnt,
        )
    } as *mut c_char;

    let mut make_cstr = |src: &OsStr| {
        let cstr = cstr_last;
        unsafe { ptr::copy_nonoverlapping(src.as_bytes().as_ptr(), cstr as *mut u8, src.len()) };
        unsafe { *cstr.add(src.len()) = 0 };
        cstr_last = unsafe { cstr_last.add(src.len() + 1) };

        cstr
    };

    unsafe { (*result_ptr).cnt = results.len() };

    // Convert all `Sym`s to `blazesym_sym`s.
    for sym in results {
        match sym {
            Symbolized::Sym(sym) => {
                let sym_ref = unsafe { &mut *syms_last };
                let name_ptr = make_cstr(OsStr::new(sym.name.as_ref()));

                sym_ref.name = name_ptr;
                sym_ref.addr = sym.addr;
                sym_ref.offset = sym.offset;
                convert_code_info(&sym.code_info, &mut sym_ref.code_info, &mut make_cstr);
                sym_ref.inlined_cnt = sym.inlined.len();
                sym_ref.inlined = inlined_last;

                for inlined in sym.inlined.iter() {
                    let inlined_ref = unsafe { &mut *inlined_last };

                    let name_ptr = make_cstr(OsStr::new(inlined.name.as_ref()));
                    inlined_ref.name = name_ptr;
                    convert_code_info(
                        &inlined.code_info,
                        &mut inlined_ref.code_info,
                        &mut make_cstr,
                    );

                    inlined_last = unsafe { inlined_last.add(1) };
                }
            }
            Symbolized::Unknown => {
                // Unknown symbols/addresses are just represented with all
                // fields set to zero.
                // SAFETY: `syms_last` is pointing to a writable and properly
                //         aligned `blaze_sym` object.
                let () = unsafe { syms_last.write_bytes(0, 1) };
            }
        }

        syms_last = unsafe { syms_last.add(1) };
    }

    result_ptr
}

unsafe fn blaze_symbolize_impl(
    symbolizer: *mut blaze_symbolizer,
    src: Source<'_>,
    inputs: Input<*const u64>,
    input_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let symbolizer = unsafe { &*symbolizer };

    let input = match inputs {
        Input::AbsAddr(addrs) => {
            // SAFETY: The caller ensures that the pointer is valid and the count
            //         matches.
            Input::AbsAddr(unsafe { slice_from_user_array(addrs, input_cnt) })
        }
        Input::VirtOffset(addrs) => {
            // SAFETY: The caller ensures that the pointer is valid and the count
            //         matches.
            Input::VirtOffset(unsafe { slice_from_user_array(addrs, input_cnt) })
        }
        Input::FileOffset(offsets) => {
            // SAFETY: The caller ensures that the pointer is valid and the count
            //         matches.
            Input::FileOffset(unsafe { slice_from_user_array(offsets, input_cnt) })
        }
    };

    let result = symbolizer.symbolize(&src, input);

    match result {
        Ok(results) if results.is_empty() => ptr::null(),
        Ok(results) => convert_symbolizedresults_to_c(results),
        Err(_err) => ptr::null(),
    }
}

/// Symbolize a list of process virtual addresses.
///
/// Return an array of [`blaze_result`] with the same size as the
/// number of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_process`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_process_virt_addrs(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_process,
    addrs: *const Addr,
    addr_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(Process::from(unsafe { &*src }));
    unsafe { blaze_symbolize_impl(symbolizer, src, Input::AbsAddr(addrs), addr_cnt) }
}

/// Symbolize a list of kernel virtual addresses.
///
/// Return an array of [`blaze_result`] with the same size as the
/// number of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_kernel`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_kernel_virt_addrs(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_kernel,
    addrs: *const Addr,
    addr_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(Kernel::from(unsafe { &*src }));
    unsafe { blaze_symbolize_impl(symbolizer, src, Input::AbsAddr(addrs), addr_cnt) }
}

/// Symbolize file addresses in an ELF file.
///
/// Return an array of [`blaze_result`] with the same size as the
/// number of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_elf`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_elf_file_addrs(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_elf,
    addrs: *const Addr,
    addr_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(Elf::from(unsafe { &*src }));
    unsafe { blaze_symbolize_impl(symbolizer, src, Input::VirtOffset(addrs), addr_cnt) }
}

/// Symbolize file addresses using "raw" Gsym data.
///
/// Return an array of [`blaze_result`] with the same size as the
/// number of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_gsym_data`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_gsym_data_file_addrs(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_gsym_data,
    addrs: *const Addr,
    addr_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid. The `GsymData`
    //         lifetime is entirely conjured up, but the object only needs to be
    //         valid for the call.
    let src = Source::from(GsymData::from(unsafe { &*src }));
    unsafe { blaze_symbolize_impl(symbolizer, src, Input::VirtOffset(addrs), addr_cnt) }
}

/// Symbolize file addresses in a Gsym file.
///
/// Return an array of [`blaze_result`] with the same size as the
/// number of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_gsym_file`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_gsym_file_file_addrs(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_gsym_file,
    addrs: *const Addr,
    addr_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(GsymFile::from(unsafe { &*src }));
    unsafe { blaze_symbolize_impl(symbolizer, src, Input::VirtOffset(addrs), addr_cnt) }
}

/// Free an array returned by any of the `blaze_symbolize_*` variants.
///
/// # Safety
/// The pointer must have been returned by any of the `blaze_symbolize_*`
/// variants.
#[no_mangle]
pub unsafe extern "C" fn blaze_result_free(results: *const blaze_result) {
    if results.is_null() {
        return;
    }

    let raw_buf_with_sz = unsafe { (results as *mut u8).offset(-(mem::size_of::<u64>() as isize)) };
    let sz = unsafe { *(raw_buf_with_sz as *mut u64) } as usize + mem::size_of::<u64>();
    unsafe { dealloc(raw_buf_with_sz, Layout::from_size_align(sz, 8).unwrap()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::CStr;
    use std::ffi::CString;
    use std::fs::read as read_file;
    use std::hint::black_box;
    use std::path::Path;
    use std::ptr;
    use std::slice;

    use blazesym::inspect;

    /// Exercise the `Debug` representation of various types.
    #[test]
    fn debug_repr() {
        let elf = blaze_symbolize_src_elf {
            path: ptr::null(),
            debug_syms: false,
        };
        assert_eq!(
            format!("{elf:?}"),
            "blaze_symbolize_src_elf { path: 0x0, debug_syms: false }"
        );

        let kernel = blaze_symbolize_src_kernel {
            kallsyms: ptr::null(),
            kernel_image: ptr::null(),
            debug_syms: true,
        };
        assert_eq!(
            format!("{kernel:?}"),
            "blaze_symbolize_src_kernel { kallsyms: 0x0, kernel_image: 0x0, debug_syms: true }"
        );

        let process = blaze_symbolize_src_process {
            pid: 1337,
            debug_syms: true,
        };
        assert_eq!(
            format!("{process:?}"),
            "blaze_symbolize_src_process { pid: 1337, debug_syms: true }"
        );

        let gsym_data = blaze_symbolize_src_gsym_data {
            data: ptr::null(),
            data_len: 0,
        };
        assert_eq!(
            format!("{gsym_data:?}"),
            "blaze_symbolize_src_gsym_data { data: 0x0, data_len: 0 }"
        );

        let gsym_file = blaze_symbolize_src_gsym_file { path: ptr::null() };
        assert_eq!(
            format!("{gsym_file:?}"),
            "blaze_symbolize_src_gsym_file { path: 0x0 }"
        );

        let sym = blaze_sym {
            name: ptr::null(),
            addr: 0x1337,
            offset: 24,
            code_info: blaze_symbolize_code_info {
                dir: ptr::null(),
                file: ptr::null(),
                line: 42,
                column: 1,
            },
            inlined_cnt: 0,
            inlined: ptr::null(),
        };
        assert_eq!(
            format!("{sym:?}"),
            "blaze_sym { name: 0x0, addr: 4919, offset: 24, code_info: blaze_symbolize_code_info { dir: 0x0, file: 0x0, line: 42, column: 1 }, inlined_cnt: 0, inlined: 0x0 }"
        );

        let inlined = blaze_symbolize_inlined_fn {
            name: ptr::null(),
            code_info: blaze_symbolize_code_info {
                dir: ptr::null(),
                file: ptr::null(),
                line: 42,
                column: 1,
            },
        };
        assert_eq!(
            format!("{inlined:?}"),
            "blaze_symbolize_inlined_fn { name: 0x0, code_info: blaze_symbolize_code_info { dir: 0x0, file: 0x0, line: 42, column: 1 } }"
        );

        let result = blaze_result { cnt: 0, syms: [] };
        assert_eq!(format!("{result:?}"), "blaze_result { cnt: 0, syms: [] }");

        let opts = blaze_symbolizer_opts {
            code_info: false,
            inlined_fns: false,
            demangle: true,
        };
        assert_eq!(
            format!("{opts:?}"),
            "blaze_symbolizer_opts { code_info: false, inlined_fns: false, demangle: true }"
        );
    }

    /// Check that we can convert a [`blaze_symbolize_src_kernel`]
    /// reference into a [`Kernel`].
    #[test]
    fn kernel_conversion() {
        let kernel = blaze_symbolize_src_kernel {
            kallsyms: ptr::null(),
            kernel_image: ptr::null(),
            debug_syms: true,
        };
        let kernel = Kernel::from(&kernel);
        assert_eq!(kernel.kallsyms, None);
        assert_eq!(kernel.kernel_image, None);

        let kernel = blaze_symbolize_src_kernel {
            kallsyms: b"/proc/kallsyms\0" as *const _ as *const c_char,
            kernel_image: b"/boot/image\0" as *const _ as *const c_char,
            debug_syms: false,
        };
        let kernel = Kernel::from(&kernel);
        assert_eq!(kernel.kallsyms, Some(PathBuf::from("/proc/kallsyms")));
        assert_eq!(kernel.kernel_image, Some(PathBuf::from("/boot/image")));
    }

    /// Test the Rust to C symbol conversion.
    #[test]
    fn symbol_conversion() {
        fn touch<X: Clone>(x: &X) {
            let x = x.clone();
            let _x = black_box(x);
        }

        fn touch_cstr(s: *const c_char) {
            if !s.is_null() {
                let s = unsafe { CStr::from_ptr(s) }.to_bytes();
                let _x = black_box(s);
            }
        }

        fn touch_code_info(code_info: &blaze_symbolize_code_info) {
            let blaze_symbolize_code_info {
                dir,
                file,
                line,
                column,
            } = code_info;

            let _x = touch_cstr(*dir);
            let _x = touch_cstr(*file);
            let _x = touch(line);
            let _x = touch(column);
        }

        /// Touch all "members" of a [`blaze_result`].
        fn touch_result(result: *const blaze_result) {
            let result = unsafe { &*result };
            for i in 0..result.cnt {
                let sym = unsafe { &*result.syms.as_slice().as_ptr().add(i) };
                let blaze_sym {
                    name,
                    addr,
                    offset,
                    code_info,
                    inlined_cnt,
                    inlined,
                } = sym;

                let () = touch_cstr(*name);
                let _x = touch(addr);
                let _x = touch(offset);
                let () = touch_code_info(code_info);

                for j in 0..*inlined_cnt {
                    let inlined_fn = unsafe { &*inlined.add(j) };
                    let blaze_symbolize_inlined_fn { name, code_info } = inlined_fn;
                    let () = touch_cstr(*name);
                    let () = touch_code_info(code_info);
                }
            }
        }

        // Empty list of symbols.
        let results = vec![];
        let result = convert_symbolizedresults_to_c(results);
        let () = touch_result(result);
        let () = unsafe { blaze_result_free(result) };

        // A single symbol with inlined function information.
        let results = vec![Symbolized::Sym(Sym {
            name: "test".into(),
            addr: 0x1337,
            offset: 0x1338,
            size: Some(42),
            code_info: Some(CodeInfo {
                dir: None,
                file: OsStr::new("a-file").into(),
                line: Some(42),
                column: Some(43),
                _non_exhaustive: (),
            }),
            inlined: vec![InlinedFn {
                name: "inlined_fn".into(),
                code_info: Some(CodeInfo {
                    dir: Some(Path::new("/some/dir").into()),
                    file: OsStr::new("another-file").into(),
                    line: Some(42),
                    column: Some(43),
                    _non_exhaustive: (),
                }),
                _non_exhaustive: (),
            }]
            .into_boxed_slice(),
            _non_exhaustive: (),
        })];
        let result = convert_symbolizedresults_to_c(results);
        let () = touch_result(result);
        let () = unsafe { blaze_result_free(result) };

        // One symbol and some unsymbolized values.
        let results = vec![
            Symbolized::Unknown,
            Symbolized::Sym(Sym {
                name: "test".into(),
                addr: 0x1337,
                offset: 0x1338,
                size: None,
                code_info: None,
                inlined: vec![InlinedFn {
                    name: "inlined_fn".into(),
                    code_info: None,
                    _non_exhaustive: (),
                }]
                .into_boxed_slice(),
                _non_exhaustive: (),
            }),
            Symbolized::Unknown,
        ];
        let result = convert_symbolizedresults_to_c(results);
        let () = touch_result(result);
        let () = unsafe { blaze_result_free(result) };
    }

    /// Make sure that we can create and free a symbolizer instance.
    #[test]
    fn symbolizer_creation() {
        let symbolizer = blaze_symbolizer_new();
        let () = unsafe { blaze_symbolizer_free(symbolizer) };
    }

    /// Make sure that we can create and free a symbolizer instance with the
    /// provided options.
    #[test]
    fn symbolizer_creation_with_opts() {
        let opts = blaze_symbolizer_opts {
            code_info: false,
            inlined_fns: false,
            demangle: true,
        };
        let symbolizer = unsafe { blaze_symbolizer_new_opts(&opts) };
        let () = unsafe { blaze_symbolizer_free(symbolizer) };
    }

    /// Make sure that we can symbolize an address using ELF, DWARF, and
    /// GSYM.
    #[test]
    fn symbolize_elf_dwarf_gsym() {
        fn test<F>(symbolize: F, has_code_info: bool)
        where
            F: FnOnce(*mut blaze_symbolizer, *const Addr, usize) -> *const blaze_result,
        {
            let symbolizer = blaze_symbolizer_new();
            let addrs = [0x2000100];
            let result = symbolize(symbolizer, addrs.as_ptr(), addrs.len());

            assert!(!result.is_null());

            let result = unsafe { &*result };
            assert_eq!(result.cnt, 1);
            let syms = unsafe { slice::from_raw_parts(result.syms.as_ptr(), result.cnt) };
            let sym = &syms[0];
            assert_eq!(
                unsafe { CStr::from_ptr(sym.name) },
                CStr::from_bytes_with_nul(b"factorial\0").unwrap()
            );
            assert_eq!(sym.addr, 0x2000100);
            assert_eq!(sym.offset, 0);

            if has_code_info {
                assert!(!sym.code_info.dir.is_null());
                assert!(!sym.code_info.file.is_null());
                assert_eq!(
                    unsafe { CStr::from_ptr(sym.code_info.file) },
                    CStr::from_bytes_with_nul(b"test-stable-addresses.c\0").unwrap()
                );
                assert_eq!(sym.code_info.line, 8);
            } else {
                assert!(sym.code_info.dir.is_null());
                assert!(sym.code_info.file.is_null());
                assert_eq!(sym.code_info.line, 0);
            }

            let () = unsafe { blaze_result_free(result) };
            let () = unsafe { blaze_symbolizer_free(symbolizer) };
        }

        let path = Path::new(&env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
            .join("test-stable-addresses-no-dwarf.bin");
        let path_c = CString::new(path.to_str().unwrap()).unwrap();
        let elf_src = blaze_symbolize_src_elf {
            path: path_c.as_ptr(),
            debug_syms: true,
        };
        let symbolize = |symbolizer, addrs, addr_cnt| unsafe {
            blaze_symbolize_elf_file_addrs(symbolizer, &elf_src, addrs, addr_cnt)
        };
        test(symbolize, false);

        let path = Path::new(&env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
            .join("test-stable-addresses-dwarf-only.bin");
        let path_c = CString::new(path.to_str().unwrap()).unwrap();
        let elf_src = blaze_symbolize_src_elf {
            path: path_c.as_ptr(),
            debug_syms: true,
        };
        let symbolize = |symbolizer, addrs, addr_cnt| unsafe {
            blaze_symbolize_elf_file_addrs(symbolizer, &elf_src, addrs, addr_cnt)
        };
        test(symbolize, true);

        let path = Path::new(&env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
            .join("test-stable-addresses.gsym");
        let path_c = CString::new(path.to_str().unwrap()).unwrap();
        let gsym_src = blaze_symbolize_src_gsym_file {
            path: path_c.as_ptr(),
        };
        let symbolize = |symbolizer, addrs, addr_cnt| unsafe {
            blaze_symbolize_gsym_file_file_addrs(symbolizer, &gsym_src, addrs, addr_cnt)
        };
        test(symbolize, true);

        let path = Path::new(&env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
            .join("test-stable-addresses.gsym");
        let data = read_file(path).unwrap();
        let gsym_src = blaze_symbolize_src_gsym_data {
            data: data.as_ptr(),
            data_len: data.len(),
        };
        let symbolize = |symbolizer, addrs, addr_cnt| unsafe {
            blaze_symbolize_gsym_data_file_addrs(symbolizer, &gsym_src, addrs, addr_cnt)
        };
        test(symbolize, true);
    }

    /// Symbolize an address inside a DWARF file, with and without auto-demangling
    /// enabled.
    #[test]
    fn symbolize_dwarf_demangle() {
        fn test(path: &Path, addr: Addr) -> Result<(), ()> {
            let opts = blaze_symbolizer_opts {
                code_info: true,
                inlined_fns: true,
                demangle: false,
            };

            let path_c = CString::new(path.to_str().unwrap()).unwrap();
            let elf_src = blaze_symbolize_src_elf {
                path: path_c.as_ptr(),
                debug_syms: true,
            };
            let symbolizer = unsafe { blaze_symbolizer_new_opts(&opts) };
            let addrs = [addr];
            let result = unsafe {
                blaze_symbolize_elf_file_addrs(symbolizer, &elf_src, addrs.as_ptr(), addrs.len())
            };
            assert!(!result.is_null());

            let result = unsafe { &*result };
            assert_eq!(result.cnt, 1);
            let syms = unsafe { slice::from_raw_parts(result.syms.as_ptr(), result.cnt) };
            let sym = &syms[0];
            let name = unsafe { CStr::from_ptr(sym.name) };
            assert!(
                name.to_str().unwrap().contains("test13test_function"),
                "{:?}",
                name
            );

            if sym.inlined_cnt == 0 {
                let () = unsafe { blaze_result_free(result) };
                let () = unsafe { blaze_symbolizer_free(symbolizer) };
                return Err(());
            }

            assert_eq!(sym.inlined_cnt, 1);
            let name = unsafe { CStr::from_ptr((*sym.inlined).name) };
            assert!(
                name.to_str().unwrap().contains("test12inlined_call"),
                "{:?}",
                name
            );

            let () = unsafe { blaze_result_free(result) };
            let () = unsafe { blaze_symbolizer_free(symbolizer) };

            // Do it again, this time with demangling enabled.
            let opts = blaze_symbolizer_opts {
                code_info: true,
                inlined_fns: true,
                demangle: true,
            };

            let symbolizer = unsafe { blaze_symbolizer_new_opts(&opts) };
            let addrs = [addr];
            let result = unsafe {
                blaze_symbolize_elf_file_addrs(symbolizer, &elf_src, addrs.as_ptr(), addrs.len())
            };
            assert!(!result.is_null());

            let result = unsafe { &*result };
            assert_eq!(result.cnt, 1);
            let syms = unsafe { slice::from_raw_parts(result.syms.as_ptr(), result.cnt) };
            let sym = &syms[0];
            assert_eq!(
                unsafe { CStr::from_ptr(sym.name) },
                CStr::from_bytes_with_nul(b"test::test_function\0").unwrap()
            );

            assert_eq!(sym.inlined_cnt, 1);
            assert_eq!(
                unsafe { CStr::from_ptr((*sym.inlined).name) },
                CStr::from_bytes_with_nul(b"test::inlined_call\0").unwrap()
            );

            let () = unsafe { blaze_result_free(result) };
            let () = unsafe { blaze_symbolizer_free(symbolizer) };
            Ok(())
        }

        let test_dwarf = Path::new(&env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("data")
            .join("test-rs.bin");
        let elf = inspect::Elf::new(&test_dwarf);
        let src = inspect::Source::Elf(elf);

        let inspector = inspect::Inspector::new();
        let results = inspector
            .lookup(&["_RNvCs69hjMPjVIJK_4test13test_function"], &src)
            .unwrap()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert!(!results.is_empty());

        let addr = results[0].addr;
        let src = Source::Elf(Elf::new(&test_dwarf));
        let symbolizer = Symbolizer::builder().enable_demangling(false).build();
        let result = symbolizer
            .symbolize_single(&src, Input::VirtOffset(addr))
            .unwrap()
            .into_sym()
            .unwrap();

        let addr = result.addr;
        let size = result.size.unwrap() as u64;
        for inst_addr in addr..addr + size {
            if test(&test_dwarf, inst_addr).is_ok() {
                return;
            }
        }

        panic!("failed to find inlined function call");
    }

    /// Make sure that we can symbolize an address in a process.
    #[test]
    fn symbolize_in_process() {
        let process_src = blaze_symbolize_src_process {
            pid: 0,
            debug_syms: true,
        };

        let symbolizer = blaze_symbolizer_new();
        let addrs = [blaze_symbolizer_new as Addr];
        let result = unsafe {
            blaze_symbolize_process_virt_addrs(
                symbolizer,
                &process_src,
                addrs.as_ptr(),
                addrs.len(),
            )
        };

        assert!(!result.is_null());

        let result = unsafe { &*result };
        assert_eq!(result.cnt, 1);
        let syms = unsafe { slice::from_raw_parts(result.syms.as_ptr(), result.cnt) };
        let sym = &syms[0];
        assert_eq!(
            unsafe { CStr::from_ptr(sym.name) },
            CStr::from_bytes_with_nul(b"blaze_symbolizer_new\0").unwrap()
        );

        let () = unsafe { blaze_result_free(result) };
        let () = unsafe { blaze_symbolizer_free(symbolizer) };
    }
}
