// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::ffi::OsStr;
use std::path::Path;

use winresource::VersionInfo;

/// The libdatadog *release* version — the workspace-wide version bumped by
/// `scripts/create-release.sh` (the "libdatadog vNN.0.0" that ships to customers
/// and that support engineers compare against a tracer's expected libdatadog
/// version) — as opposed to `CARGO_PKG_VERSION` of whichever crate's build script
/// calls [`embed_windows_version_info`]. Individual FFI crates such as
/// `libdd-profiling-ffi` pin their own, independently- and far more slowly-bumped
/// crate semver (currently frozen at `1.0.0`), which would make `FileVersion`
/// meaningless for that comparison — and, more importantly, would make it
/// *identical across every future release*, so Windows Installer's version-based
/// file-replacement logic would stop replacing this file again after the very
/// first release built with this resource, silently reproducing the bug this
/// helper exists to fix.
///
/// `build_common` itself always declares `version.workspace = true` in its own
/// `Cargo.toml`, so `env!("CARGO_PKG_VERSION")` here — resolved at *build_common's
/// own compile time*, not at the calling build script's runtime — is exactly the
/// release version we want, regardless of which crate ends up calling this
/// function.
const RELEASE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Embeds a Windows `VERSIONINFO` resource (`FileVersion`/`ProductVersion` — both
/// the string table and the binary `VS_FIXEDFILEINFO` fields `MsiGetFileVersion`
/// actually compares — plus the company/product/description string table) into
/// the artifact currently being built.
///
/// Windows Installer's file-replacement logic on upgrade is version-based: a
/// `<File>` with no `VERSIONINFO` resource makes it fall back to comparing
/// created-vs-modified timestamps, which can preserve a stale DLL across an
/// in-place upgrade.
///
/// # Cross-compilation
///
/// Call this unconditionally from `build.rs` — never guard the call with
/// `#[cfg(windows)]` / `cfg!(windows)`. Build scripts always run on the *host*, so
/// a compile-time `cfg` reflects the host platform, not whatever target Cargo is
/// building for. This function instead reads `CARGO_CFG_TARGET_OS` at
/// build-script *runtime*, which Cargo always sets to the real target, and no-ops
/// immediately when it isn't `"windows"`.
///
/// # Failure handling
///
/// Never fails the build. Cross-compiling to a Windows target from Linux/macOS CI
/// without a resource compiler on `PATH` (`llvm-rc` for the `msvc` ABI,
/// `*-windres` for the `gnu` ABI) is expected; embedding a version resource is a
/// nice-to-have, so a missing toolchain degrades to a `cargo:warning` instead of
/// aborting the build. Likewise, a `RELEASE_VERSION` that fails to parse as
/// `major.minor.patch` degrades to a warning rather than aborting; the string
/// table fields are still set correctly in that case, only the binary
/// `VS_FIXEDFILEINFO` fields fall back to whatever `WindowsResource::new()`
/// defaulted them to.
///
/// # Arguments
///
/// * `name` - the artifact's file name, with extension, e.g. `datadog_profiling_ffi.dll` for a
///   `cdylib` or `crashtracker_receiver.exe` for a binary. Cargo exposes neither `[lib] name` nor
///   the final artifact file name to build scripts, so callers spell this out literally; the
///   Windows extension is the right one to use unconditionally because this function no-ops on
///   non-Windows targets. Used verbatim for `OriginalFilename`, and with the extension removed for
///   `InternalName`.
/// * `description` - human-readable `FileDescription`, e.g. `"Datadog libdatadog FFI"`.
pub fn embed_windows_version_info(name: &str, description: &str) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set("FileVersion", RELEASE_VERSION)
        .set("ProductVersion", RELEASE_VERSION)
        .set("CompanyName", "Datadog")
        .set("ProductName", "libdatadog")
        .set("OriginalFilename", name)
        .set("InternalName", internal_name(name))
        .set("FileDescription", description)
        .set(
            "LegalCopyright",
            "Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/",
        );

    match pack_version(RELEASE_VERSION) {
        Some(packed) => {
            res.set_version_info(VersionInfo::FILEVERSION, packed);
            res.set_version_info(VersionInfo::PRODUCTVERSION, packed);
        }
        None => {
            println!(
                "cargo:warning={name}: could not parse release version {RELEASE_VERSION:?} \
                 as major.minor.patch; the binary VERSIONINFO struct will report 0.0.0.0 \
                 even though the FileVersion/ProductVersion strings are correct."
            );
        }
    }

    if let Err(err) = res.compile() {
        if cfg!(windows) {
            panic!(
                "{name}: failed to embed the Windows VERSIONINFO resource on a native Windows \
                 build ({err}); refusing to ship the artifact without it."
            );
        }
        println!(
            "cargo:warning={name}: failed to embed the Windows VERSIONINFO resource, continuing \
             without it ({err}). This is expected when cross-compiling to Windows without a \
             resource compiler (llvm-rc for the msvc ABI, *-windres for the gnu ABI) on PATH."
        );
    }
}

/// Derives the `InternalName` string-table field from an artifact file name:
/// Windows convention keeps the extension in `OriginalFilename` but leaves it out
/// of `InternalName`. Strips whatever the extension happens to be (`.dll` for a
/// `cdylib`, `.exe` for a binary, `.pyd`/`.node` for a runtime-specific extension
/// module) rather than assuming one, and returns the name unchanged when there is
/// no extension at all.
fn internal_name(file_name: &str) -> &str {
    // `file_stem` yields an `OsStr`, and the `to_str` can only fail for non-UTF-8
    // input, which a `&str` argument rules out; both fallbacks are unreachable.
    Path::new(file_name)
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or(file_name)
}

/// Packs a `major.minor.patch[-prerelease][+build]` semver string into the `u64`
/// layout winresource's [`VersionInfo::FILEVERSION`]/[`VersionInfo::PRODUCTVERSION`]
/// expect: four 16-bit words, `major << 48 | minor << 32 | patch << 16 | release`.
/// There is no separate "release"/build number in our versioning, so that last
/// word is always 0.
fn pack_version(version: &str) -> Option<u64> {
    let mut parts = version.split('.');
    let major: u16 = parts.next()?.parse().ok()?;
    let minor: u16 = parts.next()?.parse().ok()?;
    // Take only the leading digits of the patch component, in case of a semver
    // pre-release/build-metadata suffix (e.g. "1.2.3-rc.1" or "1.2.3+abc").
    let patch_str = parts.next()?;
    let patch_str = patch_str
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or(patch_str);
    let patch: u16 = patch_str.parse().ok()?;

    Some(u64::from(major) << 48 | u64::from(minor) << 32 | u64::from(patch) << 16)
}

#[cfg(test)]
mod tests {
    use super::{internal_name, pack_version};

    #[test]
    fn internal_name_strips_whatever_extension_is_present() {
        assert_eq!(
            internal_name("datadog_profiling_ffi.dll"),
            "datadog_profiling_ffi"
        );
        assert_eq!(
            internal_name("crashtracker_receiver.exe"),
            "crashtracker_receiver"
        );
        assert_eq!(internal_name("ddup.pyd"), "ddup");
        // Only the last extension goes, and a name without one is left alone.
        assert_eq!(internal_name("datadog.profiling.dll"), "datadog.profiling");
        assert_eq!(internal_name("no_extension"), "no_extension");
    }

    #[test]
    fn packs_plain_semver() {
        assert_eq!(
            pack_version("43.0.0"),
            Some(43_u64 << 48),
            "major=43, minor=0, patch=0, release=0"
        );
        assert_eq!(
            pack_version("1.2.3"),
            Some((1_u64 << 48) | (2_u64 << 32) | (3_u64 << 16))
        );
    }

    #[test]
    fn strips_prerelease_and_build_metadata_suffixes() {
        assert_eq!(pack_version("1.2.3-rc.1"), pack_version("1.2.3"));
        assert_eq!(pack_version("1.2.3+abc"), pack_version("1.2.3"));
    }

    #[test]
    fn rejects_malformed_versions() {
        assert_eq!(pack_version(""), None);
        assert_eq!(pack_version("1"), None);
        assert_eq!(pack_version("1.2"), None);
        assert_eq!(pack_version("a.b.c"), None);
    }
}
