// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Publishes the AOT-compiled Java artifact as a Maven-layout static repository. Gradle builds
// the jar (machine classes, RemoteConfig.meta, the host
// library, the provenance record) into hosts/java/build/maven-repo; this script verifies the
// wasm inputs are current, then copies the layout into the gh-pages branch of the distribution
// repository and pushes. Published versions are treated as immutable: an existing version
// directory is a hard error.
//
// Usage:
//   node scripts/publish-java-aot.mjs --repo <git-url-or-local-path> [--branch gh-pages] [--base-version 0.1.1] [--dry-run]
//
// Consumers then resolve it with one content-filtered repository line (dd-trace-java already
// declares extra repositories this way):
//   repositories { maven { url "https://bantonsson.github.io/<repo>/"; content { includeGroup("io.github.bantonsson") } } }
//   dependencies { implementation("io.github.bantonsson:remote-config-wasm-aot:<version>") }

import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))

const args = process.argv.slice(2)
function option(name) {
  const index = args.indexOf(`--${name}`)
  return index === -1 ? undefined : args[index + 1]
}
const repo = option('repo')
const branch = option('branch') ?? 'gh-pages'
const baseVersion = option('base-version')
const dryRun = args.includes('--dry-run')
if (!repo) {
  console.error('Usage: node scripts/publish-java-aot.mjs --repo <git-url-or-local-path> [--branch gh-pages] [--dry-run]')
  process.exit(1)
}

// The repository reference: either a URL (https://…, git@host:path, host:path, …) or a local
// path. URLs are passed through to git verbatim; local paths are resolved so the script
// works from any cwd.
const isUrl = repo.includes('://') || /^[^/\s]+[:@]/.test(repo)
const repoRef = isUrl ? repo : resolve(repo)

function git(command, cwd) {
  return execFileSync('git', command, { cwd, encoding: 'utf8' }).trim()
}

// --- 1. the wasm inputs must be current before anything is published -----------------------
const manifest = JSON.parse(readFileSync(`${root}/artifacts/manifest.json`, 'utf8'))
const head = git(['rev-parse', 'HEAD'], root)
if (manifest.sourceBase !== head) {
  throw new Error(`artifacts/manifest.json records ${manifest.sourceBase}, but HEAD is ${head}. ` +
    'Rebuild with `node scripts/build.mjs` so the published artifact matches the revision its version advertises.')
}
const javaWasm = readFileSync(`${root}/artifacts/remote-config-java.wasm`)
const javaSha256 = createHash('sha256').update(javaWasm).digest('hex')
if (javaSha256 !== manifest.javaSha256) {
  throw new Error(`artifacts/remote-config-java.wasm hashes to ${javaSha256}, but manifest.json ` +
    `records ${manifest.javaSha256}. Rebuild with \`node scripts/build.mjs\`.`)
}

// --- 2. build the maven layout through Gradle ----------------------------------------------
console.log(`Publishing io.github.bantonsson:remote-config-wasm-aot (wasm ${javaSha256.slice(0, 12)}…, ` +
  `libdatadog ${head.slice(0, 7)})`)
// Start from a clean staging directory so exactly the version being published is laid out.
rmSync(`${root}/hosts/java/build/maven-repo`, { recursive: true, force: true })
execFileSync('./gradlew',
    ['-q', '--no-daemon', 'publishRemoteConfigAotPublicationToStagingRepository',
        ...(baseVersion ? [`-PbaseVersion=${baseVersion}`] : [])], {
  cwd: `${root}/hosts/java`, stdio: 'inherit'
})

const groupIdPath = 'io/github/bantonsson'
const artifactPath = `${groupIdPath}/remote-config-wasm-aot`
const staging = `${root}/hosts/java/build/maven-repo`
const stagedArtifact = `${staging}/${artifactPath}`
const version = readdirSync(stagedArtifact, { withFileTypes: true })
  .filter(entry => entry.isDirectory()).map(entry => entry.name)
if (version.length !== 1) {
  throw new Error(`Expected exactly one staged version directory, found: ${version.join(', ')}`)
}
const versionName = version[0]
console.log(`Staged ${versionName}`)

// --- 3. copy into the gh-pages checkout of the distribution repository ----------------------
const checkout = mkdtempSync(join(tmpdir(), 'rc-aot-publish-'))
try {
  execFileSync('git', ['clone', '--branch', branch, repoRef, checkout], { stdio: 'inherit' })
} catch (error) {
  // The branch may not exist yet on a fresh repository; start it as an orphan.
  console.log(`Branch ${branch} not found, creating it as an orphan branch`)
  execFileSync('git', ['clone', '--no-checkout', repoRef, checkout], { stdio: 'inherit' })
  execFileSync('git', ['checkout', '--orphan', branch], { cwd: checkout, stdio: 'inherit' })
  execFileSync('git', ['rm', '-rf', '--ignore-unmatch', '.'], { cwd: checkout, stdio: 'ignore' })
}
try {
  const targetVersion = `${checkout}/${artifactPath}/${versionName}`
  if (existsSync(targetVersion) && readdirSync(targetVersion).some(name => name.endsWith('.jar'))) {
    throw new Error(`${artifactPath}/${versionName} is already published; versions are immutable. ` +
      'Bump the base version (`-PbaseVersion=…`) or publish a new libdatadog revision.')
  }
  cpSync(`${staging}/${groupIdPath}`, `${checkout}/${groupIdPath}`, { recursive: true })
  execFileSync('git', ['add', artifactPath], { cwd: checkout, stdio: 'inherit' })
  if (dryRun) {
    console.log(`Dry run: staged ${artifactPath}/${versionName} in ${checkout}, not committing`)
  } else {
    execFileSync('git', ['commit', '-m', `Publish remote-config-wasm-aot ${versionName}`], { cwd: checkout, stdio: 'inherit' })
    execFileSync('git', ['push', 'origin', `HEAD:${branch}`], { cwd: checkout, stdio: 'inherit' })
    console.log(`Pushed ${versionName} to ${branch}`)
  }
  console.log(`Consumer wiring:
  repositories { maven { url "https://bantonsson.github.io/${git(['remote', 'get-url', 'origin'], checkout).split(/[/:]/).pop().replace(/\.git$/, '')}/"; content { includeGroup("io.github.bantonsson") } } }
  dependencies { implementation("io.github.bantonsson:remote-config-wasm-aot:${versionName}") }`)
} finally {
  rmSync(checkout, { recursive: true, force: true })
}
