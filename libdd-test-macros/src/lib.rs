// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Test attribute macros for libdatadog's own test suites. Used only as a
//! path dev-dependency; never part of a shipped artifact.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{Item, ItemFn, ItemMod, spanned::Spanned};

/// Opts a test function, or an inline module of tests, into the Miri CI job.
///
/// The Miri job runs only tests whose path has a `miri_`-prefixed segment
/// (`[profile.default-miri] default-filter` in `.config/nextest.toml`). Under
/// `cfg(miri)` this attribute renames the item to `miri_<name>`; in every other
/// build the name is unchanged, so test names, `cargo test <name>` filters and
/// CI test history stay stable.
///
/// On a function it must be the outermost test-registering attribute:
///
/// ```ignore
/// #[libdd_test_macros::miri_test]
/// #[test_case(1)]
/// fn decodes(x: u8) { /* ... */ }
/// ```
///
/// `#[test]`/`#[test_case]` *above* it registers the old name first, which
/// fails to compile only under Miri. `#[tokio::test]` works in either order.
/// Attributes that stamp out several functions from one (`#[duplicate_item]`)
/// go above it. Without a test attribute, `#[test]` is added.
///
/// This crate is a dev-dependency, so the attribute must sit below any
/// `#[cfg(test)]`; a file-scope `#[test]` fn needs one added.
///
/// On a module it marks every test inside it; tests there may still opt out
/// with `#[cfg_attr(miri, ignore = "reason")]`. Paths that name the module from
/// outside (`super::tests::helper`) don't resolve under Miri, so mark the
/// functions instead when other code reaches into the module. Only inline
/// modules are supported (attribute macros on `mod foo;` are unstable).
///
/// A marked function must not also be ignored: under Miri,
/// `#[cfg_attr(miri, ignore)]` has already expanded to `#[ignore]` by the time
/// this macro runs, and the test would silently not run. That is rejected.
#[proc_macro_attribute]
pub fn miri_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(attr.into(), item.into()).into()
}

fn expand(attr: TokenStream2, item: TokenStream2) -> TokenStream2 {
    if !attr.is_empty() {
        return error(attr.span(), "#[miri_test] takes no arguments");
    }
    match syn::parse2::<Item>(item) {
        Ok(Item::Fn(f)) => mark_fn(f),
        Ok(Item::Mod(m)) => mark_mod(m),
        Ok(other) => error(
            other.span(),
            "#[miri_test] applies to a test function or an inline test module",
        ),
        Err(e) => e.to_compile_error(),
    }
}

fn mark_fn(f: ItemFn) -> TokenStream2 {
    if f.sig.ident.to_string().starts_with("miri_") {
        return error(f.sig.ident.span(), "test is already miri-marked");
    }
    if let Some(a) = f.attrs.iter().find(|a| a.path().is_ident("ignore")) {
        return error(a.span(), "a #[miri_test] test must not also be ignored");
    }
    let has_test_attr = f.attrs.iter().any(|a| {
        a.path().segments.last().is_some_and(|s| {
            matches!(
                s.ident.to_string().as_str(),
                "test" | "test_case" | "test_matrix" | "bench"
            )
        })
    });
    let test_attr = if has_test_attr {
        quote! {}
    } else {
        quote! { #[test] }
    };
    let mut marked = f.clone();
    marked.sig.ident = format_ident!("miri_{}", f.sig.ident);
    quote! {
        #[cfg(miri)] #test_attr #marked
        #[cfg(not(miri))] #test_attr #f
    }
}

fn mark_mod(m: ItemMod) -> TokenStream2 {
    if m.content.is_none() {
        return error(
            m.span(),
            "#[miri_test] on a module needs an inline module (`mod tests { ... }`); \
             mark the functions inside the file instead",
        );
    }
    if m.ident.to_string().starts_with("miri_") {
        return error(m.ident.span(), "module is already miri-marked");
    }
    let mut marked = m.clone();
    marked.ident = format_ident!("miri_{}", m.ident);
    quote! {
        #[cfg(miri)] #marked
        #[cfg(not(miri))] #m
    }
}

fn error(span: Span, msg: &str) -> TokenStream2 {
    syn::Error::new(span, msg).to_compile_error()
}

#[cfg(test)]
mod tests {
    use super::expand;
    use quote::quote;

    fn expanded(item: proc_macro2::TokenStream) -> String {
        expand(quote! {}, item).to_string()
    }

    #[test]
    fn fn_is_renamed_only_under_miri() {
        let out = expanded(quote! { #[test] fn decodes() {} });
        assert!(
            out.contains("# [cfg (miri)] # [test] fn miri_decodes"),
            "{out}"
        );
        assert!(
            out.contains("# [cfg (not (miri))] # [test] fn decodes"),
            "{out}"
        );
    }

    #[test]
    fn test_attr_is_added_when_missing() {
        let out = expanded(quote! { fn decodes() {} });
        assert_eq!(out.matches("# [test]").count(), 2, "{out}");
    }

    #[test]
    fn module_is_renamed_only_under_miri() {
        let out = expanded(quote! { mod tests { #[test] fn a() {} } });
        assert!(out.contains("# [cfg (miri)] mod miri_tests"), "{out}");
        assert!(out.contains("# [cfg (not (miri))] mod tests"), "{out}");
    }

    #[test]
    fn rejections() {
        for item in [
            quote! { #[test] #[ignore] fn a() {} },
            quote! { #[test] fn miri_a() {} },
            quote! { mod miri_tests {} },
            quote! { mod tests; },
            quote! { struct S; },
        ] {
            assert!(expanded(item.clone()).contains("compile_error"), "{item}");
        }
        assert!(
            expand(quote! { x }, quote! { fn a() {} })
                .to_string()
                .contains("compile_error")
        );
    }
}
