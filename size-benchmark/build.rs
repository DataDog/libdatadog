// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! build.rs for size-benchmark
//!
//! Scans all *-ffi/src/**/*.rs files in the workspace, finds
//! `#[no_mangle] pub [unsafe] extern "C" fn` signatures, and generates
//! `$OUT_DIR/calls.rs`.
//!
//! Generated file contains two parts:
//!
//! 1. **Callers** (functions with only ptr/primitive/Option params in public modules): Called with
//!    zeroed/null args via qualified Rust path. These force crate linking and exercise the actual
//!    function code paths.
//!
//! 2. **Symbol references** (all other discovered functions): Referenced via `extern "C"`
//!    declarations (no-arg, no-return stub) and `let _ = fn_name as *const ()`. This forces the
//!    linker to include the symbol without actually calling the function. Used for functions with
//!    complex signatures or in private modules.
//!
//! Together, these ensure all discovered FFI symbols contribute to binary size.

use quote::ToTokens;
use std::collections::HashSet;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::Path;
use syn::{FnArg, Item, ReturnType, Type};

/// (source_dir_relative_to_workspace, rust_crate_name)
const FFI_DIRS: &[(&str, &str)] = &[
    ("libdd-common-ffi/src", "libdd_common_ffi"),
    ("libdd-profiling-ffi/src", "datadog_profiling_ffi"),
    ("libdd-crashtracker-ffi/src", "libdd_crashtracker_ffi"),
    ("libdd-telemetry-ffi/src", "libdd_telemetry_ffi"),
    ("libdd-data-pipeline-ffi/src", "libdd_data_pipeline_ffi"),
    ("libdd-ddsketch-ffi/src", "libdd_ddsketch_ffi"),
    ("libdd-library-config-ffi/src", "libdd_library_config_ffi"),
    ("libdd-log-ffi/src", "libdd_log_ffi"),
    ("datadog-ffe-ffi/src", "datadog_ffe_ffi"),
    ("symbolizer-ffi/src", "symbolizer_ffi"),
    ("libdd-shared-runtime-ffi/src", "libdd_shared_runtime_ffi"),
];

fn main() {
    for (dir, _) in FFI_DIRS {
        println!("cargo:rerun-if-changed=../{dir}");
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap();

    let mut functions: Vec<FfiFunction> = Vec::new();

    for (dir, crate_name) in FFI_DIRS {
        let src_dir = workspace_root.join(dir);
        let pub_mods = collect_pub_mods(&src_dir.join("lib.rs"));
        let pattern = format!("{}/**/*.rs", src_dir.display());
        for entry in glob::glob(&pattern).unwrap().flatten() {
            if let Ok(source) = fs::read_to_string(&entry) {
                let (is_accessible, module_prefix) =
                    resolve_module(&src_dir, entry.as_path(), crate_name, &pub_mods);
                collect_ffi_functions(&source, &module_prefix, is_accessible, &mut functions);
            }
        }
    }

    // Deduplicate by function name
    functions.dedup_by(|a, b| a.name == b.name);

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("calls.rs");
    fs::write(&out_path, generate_code(&functions)).unwrap();
}

/// Returns the set of `pub mod` names declared in lib.rs.
fn collect_pub_mods(lib_rs: &Path) -> HashSet<String> {
    let mut mods = HashSet::new();
    let Ok(source) = fs::read_to_string(lib_rs) else {
        return mods;
    };
    let Ok(file) = syn::parse_file(&source) else {
        return mods;
    };
    for item in &file.items {
        if let Item::Mod(m) = item {
            if matches!(m.vis, syn::Visibility::Public(_)) {
                mods.insert(m.ident.to_string());
            }
        }
    }
    mods
}

/// Returns (is_publicly_accessible, module_prefix).
/// is_publicly_accessible = true if functions can be called via the Rust module path.
fn resolve_module(
    src_dir: &Path,
    file: &Path,
    crate_name: &str,
    pub_mods: &HashSet<String>,
) -> (bool, String) {
    let rel = file.strip_prefix(src_dir).unwrap_or(file);
    let without_ext = rel.with_extension("");
    let segments: Vec<String> = without_ext
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect();

    if segments.is_empty() || segments == ["lib"] {
        return (true, crate_name.to_string());
    }

    let path_segs: Vec<&str> = if segments.last().map(|s| s.as_str()) == Some("mod") {
        segments[..segments.len() - 1]
            .iter()
            .map(|s| s.as_str())
            .collect()
    } else {
        segments.iter().map(|s| s.as_str()).collect()
    };

    if path_segs.is_empty() {
        return (true, crate_name.to_string());
    }

    let top_mod = path_segs[0];
    let accessible = pub_mods.contains(top_mod);
    let prefix = format!("{}::{}", crate_name, path_segs.join("::"));
    (accessible, prefix)
}

struct FfiFunction {
    /// Symbol name (C name)
    name: String,
    /// Rust module path (for callable functions), e.g. `libdd_ddsketch_ffi`
    module_path: String,
    /// Callable params; None → referenced by address only (complex/inaccessible)
    params: Option<Vec<SimpleType>>,
    has_return: bool,
}

/// A simplified, trivially-constructible parameter type.
#[derive(Clone, Copy)]
enum SimpleType {
    Bool,
    Int,
    Float,
    PtrMut,
    PtrConst,
    OptionNone,
}

impl SimpleType {
    fn zero_expr(self) -> &'static str {
        match self {
            SimpleType::Bool => "false",
            SimpleType::Int => "0",
            SimpleType::Float => "0.0",
            SimpleType::PtrMut => "core::ptr::null_mut()",
            SimpleType::PtrConst => "core::ptr::null()",
            SimpleType::OptionNone => "None",
        }
    }
}

fn has_no_mangle(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("no_mangle"))
}

fn is_extern_c(abi: &Option<syn::Abi>) -> bool {
    matches!(abi, Some(syn::Abi { name: Some(n), .. }) if n.value() == "C")
}

/// Return true if any `#[cfg(...)]` attribute limits this item to a specific
/// target OS or platform that is NOT the current build target.
/// We conservatively skip items with `target_os = "windows"` on non-Windows.
fn is_platform_excluded(attrs: &[syn::Attribute]) -> bool {
    let current_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    for attr in attrs {
        if !attr.path().is_ident("cfg") {
            continue;
        }
        // Check token stream for "windows" or specific target_os values
        let tokens = attr.to_token_stream().to_string();
        if tokens.contains("windows") && current_os != "windows" {
            return true;
        }
        if tokens.contains("target_os") && tokens.contains("\"windows\"") && current_os != "windows"
        {
            return true;
        }
    }
    false
}

fn collect_ffi_functions(
    source: &str,
    module_path: &str,
    accessible: bool,
    out: &mut Vec<FfiFunction>,
) {
    let file = match syn::parse_file(source) {
        Ok(f) => f,
        Err(_) => return,
    };

    for item in &file.items {
        let Item::Fn(f) = item else { continue };

        if !matches!(f.vis, syn::Visibility::Public(_)) {
            continue;
        }
        if !is_extern_c(&f.sig.abi) {
            continue;
        }
        if !has_no_mangle(&f.attrs) {
            continue;
        }
        if is_platform_excluded(&f.attrs) {
            continue;
        }

        let name = f.sig.ident.to_string();
        let has_return = !matches!(f.sig.output, ReturnType::Default);

        // Try to build callable params (only for accessible modules)
        let params = if accessible {
            let mut p: Vec<SimpleType> = Vec::new();
            let mut skip = false;
            for arg in &f.sig.inputs {
                let FnArg::Typed(pat_type) = arg else {
                    continue;
                };
                match simplify_type(&pat_type.ty) {
                    Some(st) => p.push(st),
                    None => {
                        skip = true;
                        break;
                    }
                }
            }
            if skip {
                None
            } else {
                Some(p)
            }
        } else {
            None
        };

        out.push(FfiFunction {
            name,
            module_path: module_path.to_string(),
            params,
            has_return,
        });
    }
}

/// Map a syn::Type to a SimpleType, or None to skip this function.
fn simplify_type(ty: &Type) -> Option<SimpleType> {
    match ty {
        Type::Ptr(p) => Some(if p.mutability.is_some() {
            SimpleType::PtrMut
        } else {
            SimpleType::PtrConst
        }),
        // References require valid data — skip
        Type::Reference(_) => None,
        Type::BareFn(_) => Some(SimpleType::PtrMut),
        Type::Path(tp) => {
            if tp.qself.is_some() {
                return None;
            }
            let last = tp.path.segments.last()?.ident.to_string();
            match last.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64"
                | "i128" | "isize" | "c_int" | "c_uint" | "c_long" | "c_ulong" | "c_short"
                | "c_ushort" | "c_char" | "c_schar" | "c_uchar" | "c_longlong" | "c_ulonglong" => {
                    Some(SimpleType::Int)
                }
                "f32" | "f64" | "c_float" | "c_double" => Some(SimpleType::Float),
                "bool" => Some(SimpleType::Bool),
                "Option" => Some(SimpleType::OptionNone),
                _ => None,
            }
        }
        _ => None,
    }
}

fn generate_code(functions: &[FfiFunction]) -> String {
    let mut code = String::new();
    writeln!(
        code,
        "// Auto-generated by size-benchmark/build.rs — DO NOT EDIT"
    )
    .unwrap();
    writeln!(code).unwrap();

    // Collect functions that need extern "C" stubs (not callable via Rust path)
    let needs_extern: Vec<&FfiFunction> = functions.iter().filter(|f| f.params.is_none()).collect();

    if !needs_extern.is_empty() {
        writeln!(code, "extern \"C\" {{").unwrap();
        for f in &needs_extern {
            // Declare with no params/return — we only take the address, not call it
            writeln!(code, "    fn {}();", f.name).unwrap();
        }
        writeln!(code, "}}").unwrap();
        writeln!(code).unwrap();
    }

    writeln!(code, "pub fn exercise_all() {{").unwrap();
    writeln!(code, "    #[allow(unused_unsafe)]").unwrap();
    writeln!(code, "    unsafe {{").unwrap();

    let mut called = 0u32;
    let mut referenced = 0u32;

    for f in functions {
        match &f.params {
            Some(params) => {
                // Call via qualified Rust path
                let args: Vec<String> = params
                    .iter()
                    .map(|st| format!("std::hint::black_box({})", st.zero_expr()))
                    .collect();
                let call = format!("{}::{}", f.module_path, f.name);
                let invocation = format!("{}({})", call, args.join(", "));
                if f.has_return {
                    writeln!(
                        code,
                        "        let _ = std::hint::black_box({});",
                        invocation
                    )
                    .unwrap();
                } else {
                    writeln!(code, "        {};", invocation).unwrap();
                }
                called += 1;
            }
            None => {
                // Reference via address-of to force linker inclusion
                writeln!(
                    code,
                    "        let _ = std::hint::black_box({} as *const ());",
                    f.name
                )
                .unwrap();
                referenced += 1;
            }
        }
    }

    writeln!(code, "    }}").unwrap();
    writeln!(code, "}}").unwrap();
    writeln!(code).unwrap();
    writeln!(
        code,
        "// Coverage: {called} functions called (with args), {referenced} referenced (address-only)"
    )
    .unwrap();
    code
}
