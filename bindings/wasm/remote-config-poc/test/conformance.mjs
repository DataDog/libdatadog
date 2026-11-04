// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import http from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

import { RcAbi } from '../hosts/node/bindings.mjs'
import { input, load, poll } from '../hosts/node/client.mjs'

import { agentlessFixture } from './agentless-fixture.mjs'

const root = new URL('../', import.meta.url)
const wasm = fileURLToPath(new URL('artifacts/remote-config.wasm', root))
const scenarioPath = fileURLToPath(new URL('test/scenario.json', root))
const scenario = JSON.parse(readFileSync(scenarioPath, 'utf8'))
const configPath = 'datadog/2/APM_TRACING/poc/config'
const contents = ['{"enabled":false}', '{"enabled":true}', '{"sample_rate":0.5}']

/** @param {number} version @param {string} [content] @param {boolean} [corrupt] */
function response (version, content, corrupt = false) {
  const targets = content === undefined ? {} : {
    [configPath]: {
      custom: { v: version },
      hashes: { sha256: corrupt ? '0'.repeat(64) : createHash('sha256').update(content).digest('hex') },
      length: Buffer.byteLength(content)
    }
  }
  return {
    roots: [],
    targets: Buffer.from(JSON.stringify({ signed: {
      custom: { opaque_backend_state: 'poc-state', agent_refresh_interval: 1000 }, targets, version
    } })).toString('base64'),
    target_files: content === undefined ? [] : [{ path: configPath, raw: Buffer.from(content).toString('base64') }],
    client_configs: content === undefined ? [] : [configPath]
  }
}

/** @param {string} command @param {string[]} args */
async function execute (command, args) {
  const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'] })
  const stdout = []
  const stderr = []
  child.stdout.on('data', chunk => stdout.push(chunk))
  child.stderr.on('data', chunk => stderr.push(chunk))
  const [code] = await once(child, 'close')
  assert.equal(code, 0, Buffer.concat(stderr).toString())
  return Buffer.concat(stdout).toString().trim().split('\n').map(line => JSON.parse(line))
}

for (const language of process.env.LANGUAGES?.split(',') ?? ['nodejs', 'dotnet', 'java']) {
  test(`${language}: agentless TUF signatures, expiry, file integrity, and recovery`, async () => {
    const fixture = agentlessFixture(scenario)
    const directory = mkdtempSync(join(tmpdir(), 'libdd-agentless-'))
    const path = join(directory, 'scenario.json')
    writeFileSync(path, JSON.stringify(fixture))
    try {
      const results = language === 'nodejs'
        ? await execute(process.execPath, [fileURLToPath(new URL('hosts/node/demo.mjs', root)), wasm, fixture.options.url, path])
        : language === 'java'
        ? await execute('java', ['-jar', fileURLToPath(new URL('hosts/java/build/libs/remote-config-host.jar', root)), wasm, fixture.options.url, path])
        : await execute('dotnet', [fileURLToPath(new URL('hosts/dotnet/bin/Release/net10.0/RemoteConfigPoc.dll', root)), wasm, fixture.options.url, path])
      assert.equal(results.length, 9)
      assert.equal(results[0].error, 'cancelled')
      const outcomes = results.slice(1)
      assert.equal(outcomes[0].outcome.changes[0].kind, 'add')
      assert.equal(outcomes[0].outcome.changes[0].version, '1')
      assert.equal(outcomes[0].outcome.changes[0].contentsBase64, Buffer.from('{"sample_rate":0.1}').toString('base64'))
      assert.match(outcomes[2].outcome.error, /signature|verif|threshold/i)
      assert.equal(outcomes[3].outcome.changes[0].kind, 'update')
      assert.equal(outcomes[3].outcome.changes[0].version, '2')
      assert.match(outcomes[4].outcome.error, /hash/i)
      assert.equal(outcomes[5].outcome.changes[0].version, '3')
      assert.match(outcomes[6].outcome.error, /expir/i)
      assert.equal(outcomes[7].outcome.changes[0].version, '4')
      const requests = outcomes.flatMap(value => value.requests ?? [])
      assert.ok(requests.some(request => new URL(request.url).pathname === '/api/v0.1/org'))
      for (const request of requests) {
        assert.equal(new URL(request.url).hostname, 'config.poc.invalid')
        assert.ok(request.headers.some(([name, value]) => name === 'dd-api-key' && value === 'poc-api-key'))
        assert.ok(request.headers.some(([name, value]) => name === 'content-type' && value === 'application/x-protobuf'))
        if (request.method === 'POST') {
          const body = Buffer.from(request.body, 'base64')
          assert.ok(body.includes(Buffer.from(language)))
          assert.ok(body.includes(Buffer.from('poc-host')))
        }
      }
    } finally {
      rmSync(directory, { recursive: true })
    }
  })

  test(`${language}: real RC protocol, acknowledgements, cancellation, timeout, and hash verification`, async () => {
    const requests = []
    const errors = []
    const responses = [response(1, contents[0]), response(2, contents[1]), {}, response(3),
      response(4, contents[2], true), response(4, contents[2]), undefined]
    const server = http.createServer(async (request, reply) => {
      try {
        const chunks = []
        for await (const chunk of request) chunks.push(chunk)
        assert.equal(request.method, 'POST')
        assert.equal(request.url, '/v0.7/config')
        const body = JSON.parse(Buffer.concat(chunks).toString())
        assert.equal(body.client.client_tracer.language, language)
        assert.equal(body.client.client_tracer.service, 'shared-service')
        assert.equal(body.client.id, 'shared-wasm-poc')
        const index = requests.push(body) - 1
        assert.ok(index < responses.length, 'Unexpected extra poll')
        reply.statusCode = responses[index] === undefined ? 404 : 200
        reply.end(JSON.stringify(responses[index] ?? {}))
      } catch (error) {
        errors.push(error)
        reply.statusCode = 500
        reply.end('Fixture failed')
      }
    })
    server.listen(0, '127.0.0.1')
    await once(server, 'listening')
    try {
      const url = `http://127.0.0.1:${server.address().port}`
      const outcomes = language === 'nodejs'
        ? await execute(process.execPath, [fileURLToPath(new URL('hosts/node/demo.mjs', root)), wasm, url, scenarioPath])
        : language === 'java'
        ? await execute('java', ['-jar', fileURLToPath(new URL('hosts/java/build/libs/remote-config-host.jar', root)), wasm, url, scenarioPath])
        : await execute('dotnet', [fileURLToPath(new URL('hosts/dotnet/bin/Release/net10.0/RemoteConfigPoc.dll', root)), wasm, url, scenarioPath])
      assert.deepEqual(errors, [])
      assert.equal(requests.length, 7, JSON.stringify(outcomes))
      assert.equal(outcomes.length, 11)
      assert.equal(outcomes[0].changes[0].kind, 'add')
      assert.equal(outcomes[0].changes[0].version, '1')
      assert.equal(outcomes[0].changes[0].contentsBase64, Buffer.from(contents[0]).toString('base64'))
      assert.equal(outcomes[2].error, 'cancelled')
      assert.match(outcomes[3].error, /timed out/)
      assert.equal(outcomes[4].changes[0].kind, 'update')
      assert.equal(outcomes[4].changes[0].version, '2')
      assert.deepEqual(outcomes[6], { changes: [] })
      assert.equal(outcomes[7].changes[0].kind, 'remove')
      assert.deepEqual(outcomes[8], { changes: [] }, 'A corrupt file must never reach the consumer')
      assert.equal(outcomes[9].changes[0].kind, 'add')
      assert.equal(outcomes[10].changes[0].kind, 'remove')
      assert.equal(requests[1].cached_target_files[0].path, configPath)
      assert.equal(requests[1].client.state.config_states[0].apply_state, 2)
      assert.equal(requests[2].client.state.config_states[0].apply_state, 3)
      assert.equal(requests[2].client.state.config_states[0].apply_error, 'consumer rejected config')
      assert.equal(requests[5].client.state.has_error, true)
      assert.match(requests[5].client.state.error, /hash/i)
    } finally {
      server.closeAllConnections()
      await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
    }
  })
}

test('artifact has only WASI imports and generated ABI exports', () => {
  const bytes = readFileSync(wasm)
  const module = new WebAssembly.Module(bytes)
  const imports = WebAssembly.Module.imports(module)
  assert.ok(imports.length > 0)
  assert.ok(imports.every(entry => entry.module === 'wasi_snapshot_preview1'))
  const exports = WebAssembly.Module.exports(module)
  for (const name of Object.keys(JSON.parse(readFileSync(new URL('abi.json', root))).exports)) {
    assert.ok(exports.some(entry => entry.name === name && entry.kind === 'function'), name)
  }
  process.stdout.write(`WASM sha256 ${createHash('sha256').update(bytes).digest('hex')} (${bytes.length} bytes)\n`)
})

test('buffer ownership, malformed options, duplicate operations, and disposal', () => {
  const abi = load(wasm)
  assert.equal(abi.exports.rc_alloc(16 * 1024 * 1024 + 1), -1)
  assert.equal(abi.exports.rc_alloc(-1), -1)
  const boundary = abi.check(abi.exports.rc_alloc(16 * 1024 * 1024))
  abi.check(abi.exports.rc_free(boundary))
  const buffer = abi.write({})
  assert.equal(abi.exports.rc_create(buffer), -1)
  assert.throws(() => abi.check(-1), /missing field/)
  abi.check(abi.exports.rc_free(buffer))
  assert.equal(abi.exports.rc_free(buffer), -1)
  const options = { ...scenario.options, language: 'nodejs', url: 'http://127.0.0.1:8126' }
  assert.throws(() => input(abi, { ...options, apiKey: 'key', hostname: 'host' }, abi.exports.rc_create), /https/i)
  assert.throws(() => input(abi, { ...options, url: 'https://poc.invalid', apiKey: 'key' }, abi.exports.rc_create), /hostname/i)
  assert.throws(() => input(abi, { ...options, url: 'https://poc.invalid', apiKey: '', hostname: 'host' }, abi.exports.rc_create), /empty/)
  assert.throws(() => input(abi, { ...options, configRoot: '{}' }, abi.exports.rc_create), /require an API key/)
  const invalidRoot = input(abi, { ...options, url: 'https://poc.invalid', apiKey: 'key',
    hostname: 'host', configRoot: '{' }, abi.exports.rc_create)
  try {
    assert.throws(() => input(abi, { path: configPath, state: 2 }, data => abi.exports.rc_ack(invalidRoot, data)), /not initialized/)
    for (let attempt = 0; attempt < 2; attempt++) {
      abi.check(abi.exports.rc_poll(invalidRoot))
      assert.match(abi.json(abi.exports.rc_result(invalidRoot)).error, /root|parse|json|EOF/i)
      assert.deepEqual(abi.json(abi.exports.rc_events(invalidRoot)), [])
    }
  } finally {
    abi.check(abi.exports.rc_close(invalidRoot))
  }
  const maxTimer = input(abi, { ...options, timeoutMs: 2_147_483_647 }, abi.exports.rc_create)
  abi.check(abi.exports.rc_close(maxTimer))
  assert.throws(() => input(abi, { ...options, timeoutMs: 2_147_483_648 }, abi.exports.rc_create), /timer range/)
  const client = input(abi, { ...scenario.options, language: 'nodejs', url: 'http://127.0.0.1:8126' }, abi.exports.rc_create)
  abi.check(abi.exports.rc_poll(client))
  assert.equal(abi.exports.rc_poll(client), -1)
  assert.throws(() => input(abi, { path: configPath, state: 2 }, data => abi.exports.rc_ack(client, data)), /active poll/)
  abi.check(abi.exports.rc_cancel(client))
  assert.equal(abi.json(abi.exports.rc_result(client)).error, 'cancelled')
  abi.check(abi.exports.rc_close(client))
  assert.equal(abi.exports.rc_poll(client), -1)
  assert.equal(abi.exports.rc_close(client), -1)
  assert.throws(() => new RcAbi({ ...abi.exports, rc_version: () => 2 }), /version mismatch/)
})

test('.NET adapter cancels in-flight HTTP through CancellationToken and can poll again', async () => {
  const server = http.createServer((_request, response) => {
    response.once('close', () => server.emit('aborted-response'))
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const observed = once(server, 'request')
  const aborted = once(server, 'aborted-response')
  const child = spawn('dotnet', [
    fileURLToPath(new URL('hosts/dotnet/bin/Release/net10.0/RemoteConfigPoc.dll', root)),
    wasm, `http://127.0.0.1:${server.address().port}`, scenarioPath, '--cancel-stdin'
  ], { stdio: ['pipe', 'pipe', 'pipe'] })
  const output = []
  const errors = []
  child.stdout.on('data', chunk => output.push(chunk))
  child.stderr.on('data', chunk => errors.push(chunk))
  const finished = once(child, 'close')
  try {
    await observed
    child.stdin.end('cancel\n')
    const [code] = await finished
    assert.equal(code, 0, Buffer.concat(errors).toString())
    assert.deepEqual(Buffer.concat(output).toString().trim().split('\n').map(line => JSON.parse(line)), [
      { error: 'cancelled' }, { restarted: true }
    ])
    await aborted
  } finally {
    child.kill()
    server.closeAllConnections()
    await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  }
})

test('Java adapter cancels in-flight HTTP and can poll again', async () => {
  const server = http.createServer((_request, response) => {
    response.once('close', () => server.emit('aborted-response'))
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const observed = once(server, 'request')
  const aborted = once(server, 'aborted-response')
  const child = spawn('java', [
    '-jar', fileURLToPath(new URL('hosts/java/build/libs/remote-config-host.jar', root)),
    wasm, `http://127.0.0.1:${server.address().port}`, scenarioPath, '--cancel-stdin'
  ], { stdio: ['pipe', 'pipe', 'pipe'] })
  const output = []
  const errors = []
  child.stdout.on('data', chunk => output.push(chunk))
  child.stderr.on('data', chunk => errors.push(chunk))
  const finished = once(child, 'close')
  try {
    await observed
    child.stdin.end('cancel\n')
    const [code] = await finished
    assert.equal(code, 0, Buffer.concat(errors).toString())
    assert.deepEqual(Buffer.concat(output).toString().trim().split('\n').map(line => JSON.parse(line)), [
      { error: 'cancelled' }, { restarted: true }
    ])
    await aborted
  } finally {
    child.kill()
    server.closeAllConnections()
    await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  }
})

test('Node adapter aborts an in-flight HTTP request and can poll again', async () => {
  const server = http.createServer((_request, response) => {
    response.once('close', () => server.emit('aborted-response'))
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const abi = load(wasm)
  const client = input(abi, { ...scenario.options, language: 'nodejs', url: `http://127.0.0.1:${server.address().port}` }, abi.exports.rc_create)
  try {
    const seen = once(server, 'request')
    const aborted = once(server, 'aborted-response')
    const controller = new AbortController()
    const completion = poll(abi, client, controller.signal)
    await seen
    controller.abort()
    assert.equal((await completion).error, 'cancelled')
    await aborted
    abi.check(abi.exports.rc_poll(client))
    abi.check(abi.exports.rc_cancel(client))
    assert.equal(abi.json(abi.exports.rc_result(client)).error, 'cancelled')
  } finally {
    abi.check(abi.exports.rc_close(client))
    server.closeAllConnections()
    await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
  }
})
