// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const toolchain = readFileSync(`${root}/rust-toolchain.toml`, 'utf8').match(/^channel = "([^"]+)"/m)[1]
const rustc = execFileSync('rustup', ['which', '--toolchain', toolchain, 'rustc'], { encoding: 'utf8' }).trim()
const env = { ...process.env, RUSTC: rustc }
const llvmBin = process.env.LLVM_BIN ?? (process.platform === 'darwin'
  ? `${process.arch === 'arm64' ? '/opt/homebrew' : '/usr/local'}/opt/llvm/bin/`
  : '')
env.CC_wasm32_wasip1 ??= `${llvmBin}clang`
env.AR_wasm32_wasip1 ??= `${llvmBin}llvm-ar`
const wasmOpt = process.env.WASM_OPT ?? 'wasm-opt'
const optimizerVersion = execFileSync(wasmOpt, ['--version'], { encoding: 'utf8' }).trim()
if (!/^wasm-opt version 133\b/.test(optimizerVersion)) throw new Error('Binaryen wasm-opt version 133 is required')
execFileSync(process.execPath, ['scripts/generate.mjs'], { cwd: root, stdio: 'inherit' })
execFileSync('rustup', ['run', toolchain, 'cargo', 'build', '--target', 'wasm32-wasip1', '--release', '--locked'], {
  cwd: root, env, stdio: 'inherit'
})
const metadata = JSON.parse(execFileSync('rustup', ['run', toolchain, 'cargo', 'metadata', '--locked', '--format-version', '1'], {
  cwd: root, env, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024
}))
const licenses = metadata.packages.filter(entry => entry.source).map(entry =>
  [entry.name, entry.version, entry.license, entry.repository ?? ''].map(value => JSON.stringify(value ?? '')).join(',')
).sort()
writeFileSync(`${root}/LICENSE-3rdparty.csv`, 'name,version,license,repository\n' + licenses.join('\n') + '\n')
mkdirSync(`${root}/artifacts`, { recursive: true })
const source = `${root}/target/wasm32-wasip1/release/libdd_remote_config_wasm_poc.wasm`
const artifact = `${root}/artifacts/remote-config.wasm`
execFileSync(wasmOpt, [source, '-Oz', '--converge', '--enable-mutable-globals',
  '--enable-nontrapping-float-to-int', '--enable-bulk-memory', '--enable-sign-ext',
  '--enable-reference-types', '--enable-multivalue', '--disable-gc', '-o', artifact], { stdio: 'inherit' })
const bytes = readFileSync(artifact)
const manifest = {
  abiVersion: JSON.parse(readFileSync(`${root}/abi.json`)).version,
  sourceBase: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  target: 'wasm32-wasip1',
  rustToolchain: toolchain,
  optimizer: optimizerVersion,
  sha256: createHash('sha256').update(bytes).digest('hex'),
  bytes: bytes.length
}
writeFileSync(`${root}/artifacts/manifest.json`, JSON.stringify(manifest, null, 2) + '\n')
process.stdout.write(`Shared artifact: ${manifest.sha256} (${manifest.bytes} bytes)\n`)
execFileSync('dotnet', ['build', 'hosts/dotnet/RemoteConfigPoc.csproj', '-c', 'Release', '--nologo', '-p:RestoreLockedMode=true'], {
  cwd: root, stdio: 'inherit'
})
// The Java host AOT-compiles the final artifact into JVM bytecode at build time.
execFileSync('./gradlew', ['-q', '--no-daemon', 'build'], { cwd: `${root}/hosts/java`, stdio: 'inherit' })
