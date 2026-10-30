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

/// Environment variable through which a packaging layer that redistributes these
/// artifacts under a version of its own overrides `FileVersion`.
///
/// `libdatadog-dotnet` is the motivating case: it pins an upstream libdatadog
/// version in a file and tags its own releases independently, so two of its
/// releases can ship genuinely different DLLs — different feature sets, different
/// code — built from the same upstream version. Those DLLs would be
/// indistinguishable by [`RELEASE_VERSION`], and Windows Installer's default
/// `REINSTALLMODE` of `omus` only overwrites a file whose version is *older*, so
/// it would decline to replace one with the other.
///
/// Only `FileVersion` is overridden, since that is the field Windows Installer
/// compares; `ProductVersion` keeps [`RELEASE_VERSION`] so the upstream libdatadog
/// version a DLL was built from stays recoverable from the shipped file.
///
/// # Setting it
///
/// Nothing needs to be set to build libdatadog, including for Windows: unset means
/// `FileVersion` is [`RELEASE_VERSION`], which is what every libdatadog release
/// ships and what CI builds. It is only for redistributors, who have two equivalent
/// entry points — `builder` does nothing more than forward the flag to the cargo
/// invocation that builds the artifacts:
///
/// ```text
/// # through the release binary, which is what redistributors invoke
/// release --out <dir> --target x86_64-pc-windows-msvc --file-version 1.3.5
///
/// # or through the environment, when driving cargo directly
/// LIBDATADOG_PACKAGE_VERSION=1.3.5 cargo build -p libdd-profiling-ffi \
///     --target x86_64-pc-windows-msvc
/// ```
const FILE_VERSION_VAR: &str = "LIBDATADOG_PACKAGE_VERSION";

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
/// # Which version goes where
///
/// `ProductVersion` is always the libdatadog release version — the workspace-wide
/// version that `vNN.0.0` tags carry — identifying the release the artifact was
/// built from. `FileVersion` is the same by default, but a redistributor that
/// ships these artifacts under a version of its own can override it by setting
/// `LIBDATADOG_PACKAGE_VERSION`: that is the field Windows Installer compares, so
/// it has to be whichever version actually changes whenever the shipped bytes do.
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
/// Fails the build, by design. Once the early return above has established that
/// the *target* is Windows, every remaining failure is a broken build
/// environment rather than something a caller can be expected to tolerate: we
/// build all Windows artifacts natively on Windows runners, and a resource
/// compiler is part of the same toolchain that links the artifact in the first
/// place, so it is present whenever linking can succeed at all. Degrading to a
/// `cargo:warning` here would silently produce exactly the artifact this helper
/// exists to prevent — one that an MSI upgrade compares by timestamp and may
/// skip replacing — and a warning scrolling past a green build is not a signal
/// anyone acts on.
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
    if !matches!(env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("windows")) {
        return;
    }

    println!("cargo:rerun-if-env-changed={FILE_VERSION_VAR}");
    let override_var = env::var(FILE_VERSION_VAR);
    let (file_version, file_version_source) = select_file_version(override_var.as_deref().ok());

    let mut res = winresource::WindowsResource::new();
    res.set("FileVersion", file_version)
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

    res.set_version_info(
        VersionInfo::FILEVERSION,
        pack_or_fail(name, file_version, file_version_source),
    );
    res.set_version_info(
        VersionInfo::PRODUCTVERSION,
        pack_or_fail(name, RELEASE_VERSION, "the libdatadog release version"),
    );

    if let Err(err) = res.compile() {
        panic!(
            "{name}: failed to embed the Windows VERSIONINFO resource ({err}); refusing to \
             ship a Windows artifact without one. A resource compiler must be on PATH: \
             `rc.exe`/`llvm-rc` (shipped with the Windows SDK) for the `msvc` ABI, \
             `*-windres` (shipped with mingw) for the `gnu` ABI."
        );
    }
}

/// Picks the `FileVersion` from the raw value of [`FILE_VERSION_VAR`], returning it
/// alongside a description of where it came from for use in error messages. An
/// exported-but-empty variable is how CI spells "unset", so it is treated as such
/// rather than failing the build on an unparseable empty version.
fn select_file_version(raw_override: Option<&str>) -> (&str, &str) {
    match raw_override {
        Some(version) if !version.is_empty() => (version, FILE_VERSION_VAR),
        _ => (RELEASE_VERSION, "the libdatadog release version"),
    }
}

/// Packs `version` for one of the binary `VS_FIXEDFILEINFO` fields, failing the
/// build if it is not `major.minor.patch`. `source` names where the version came
/// from, so the error points at the workspace version or at the overriding
/// environment variable rather than leaving the caller to guess.
fn pack_or_fail(name: &str, version: &str, source: &str) -> u64 {
    match pack_version(version) {
        Some(packed) => packed,
        None => panic!(
            "{name}: could not parse {source} ({version:?}) as major.minor.patch. The binary \
             VS_FIXEDFILEINFO fields are what Windows Installer compares to decide whether to \
             replace this file on upgrade, so they cannot be left to default to 0.0.0.0."
        ),
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
    use super::{
        FILE_VERSION_VAR, RELEASE_VERSION, internal_name, pack_version, select_file_version,
    };

    #[test]
    fn file_version_falls_back_to_the_release_version() {
        let release = (RELEASE_VERSION, "the libdatadog release version");
        assert_eq!(select_file_version(None), release);
        // An exported-but-empty variable must not be mistaken for an override.
        assert_eq!(select_file_version(Some("")), release);
    }

    #[test]
    fn file_version_prefers_a_redistributor_override() {
        assert_eq!(
            select_file_version(Some("1.3.5")),
            ("1.3.5", FILE_VERSION_VAR)
        );
    }

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
