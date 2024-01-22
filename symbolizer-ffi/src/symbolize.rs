// This file is a forward of the ffi interface of blazesym::symbolize.
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
use blazesym::Addr;
use std::ptr::NonNull;
use std::slice;

/// # Safety
/// This function is unsafe because it creates a slice from a raw pointer.
/// The caller must ensure that `items` points to a valid memory location
/// and `num_items` does not exceed the number of elements `items` points to.
pub unsafe fn slice_from_user_array<'t, T>(items: *const T, num_items: usize) -> &'t [T] {
    let items = if items.is_null() {
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
#[derive(Debug)]
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
pub(crate) unsafe fn from_cstr(cstr: *const c_char) -> PathBuf {
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
        Symbolized::Unknown(..) => acc,
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
            Symbolized::Unknown(..) => {
                // Unknown symbols/addresses are just represented with all
                // fields set to zero.
                // SAFETY: `syms_last` is pointing to a writable and properly
                //         aligned `blaze_sym` object.
                unsafe { syms_last.write_bytes(0, 1) };
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

/// Symbolize a list of process absolute addresses.
///
/// Return an array of [`blaze_result`] with the same size as the number
/// of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_process`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_process_abs_addrs(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_process,
    abs_addrs: *const Addr,
    abs_addr_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(Process::from(unsafe { &*src }));
    unsafe { blaze_symbolize_impl(symbolizer, src, Input::AbsAddr(abs_addrs), abs_addr_cnt) }
}

/// Symbolize a list of kernel absolute addresses.
///
/// Return an array of [`blaze_result`] with the same size as the number
/// of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_kernel`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_kernel_abs_addrs(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_kernel,
    abs_addrs: *const Addr,
    abs_addr_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(Kernel::from(unsafe { &*src }));
    unsafe { blaze_symbolize_impl(symbolizer, src, Input::AbsAddr(abs_addrs), abs_addr_cnt) }
}

/// Symbolize virtual offsets in an ELF file.
///
/// Return an array of [`blaze_result`] with the same size as the number
/// of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_elf`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_elf_virt_offsets(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_elf,
    virt_offsets: *const Addr,
    virt_offset_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(Elf::from(unsafe { &*src }));
    unsafe {
        blaze_symbolize_impl(
            symbolizer,
            src,
            Input::VirtOffset(virt_offsets),
            virt_offset_cnt,
        )
    }
}

/// Symbolize virtual offsets using "raw" Gsym data.
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
pub unsafe extern "C" fn blaze_symbolize_gsym_data_virt_offsets(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_gsym_data,
    virt_offsets: *const Addr,
    virt_offset_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid. The `GsymData`
    //         lifetime is entirely conjured up, but the object only needs to be
    //         valid for the call.
    let src = Source::from(GsymData::from(unsafe { &*src }));
    unsafe {
        blaze_symbolize_impl(
            symbolizer,
            src,
            Input::VirtOffset(virt_offsets),
            virt_offset_cnt,
        )
    }
}

/// Symbolize virtual offsets in a Gsym file.
///
/// Return an array of [`blaze_result`] with the same size as the number
/// of input addresses. The caller should free the returned array by
/// calling [`blaze_result_free`].
///
/// # Safety
/// `symbolizer` must have been allocated using [`blaze_symbolizer_new`] or
/// [`blaze_symbolizer_new_opts`]. `src` must point to a valid
/// [`blaze_symbolize_src_gsym_file`] object. `addrs` must represent an array of
/// `addr_cnt` objects.
#[no_mangle]
pub unsafe extern "C" fn blaze_symbolize_gsym_file_virt_offsets(
    symbolizer: *mut blaze_symbolizer,
    src: *const blaze_symbolize_src_gsym_file,
    virt_offsets: *const Addr,
    virt_offset_cnt: usize,
) -> *const blaze_result {
    // SAFETY: The caller ensures that the pointer is valid.
    let src = Source::from(GsymFile::from(unsafe { &*src }));
    unsafe {
        blaze_symbolize_impl(
            symbolizer,
            src,
            Input::VirtOffset(virt_offsets),
            virt_offset_cnt,
        )
    }
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
