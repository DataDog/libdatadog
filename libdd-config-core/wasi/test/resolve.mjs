// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { WASI } from 'node:wasi'

const wasi = new WASI({
  version: 'preview1',
  preopens: { '/config': fileURLToPath(new URL('../../tests/fixtures/', import.meta.url)) },
})
const wasm = new WebAssembly.Module(readFileSync(process.argv[2]))
const instance = new WebAssembly.Instance(wasm, wasi.getImportObject())
wasi.initialize(instance)

const {
  memory,
  config_v1_alloc: allocate,
  config_v1_resolve: resolve,
  config_v1_free: free,
} = instance.exports

function request(operation) {
  const payload = Buffer.from(JSON.stringify({ version: 1, ...operation }))
  const input = allocate(payload.length)
  const descriptor = allocate(8)
  assert.ok(input)
  assert.ok(descriptor)
  new Uint8Array(memory.buffer, input, payload.length).set(payload)
  assert.equal(resolve(input, payload.length + 1, descriptor), 1)
  assert.equal(resolve(input, payload.length, descriptor), 0)

  const view = new DataView(memory.buffer, descriptor, 8)
  const pointer = view.getUint32(0, true)
  const length = view.getUint32(4, true)
  try {
    return JSON.parse(Buffer.from(memory.buffer, pointer, length))
  } finally {
    assert.equal(free(pointer), 0)
    assert.equal(free(input), 0)
    assert.equal(free(descriptor), 0)
  }
}

const initialization = {
  operation: 'initialize',
  manifest: { kind: 'path', value: '/config/supported-configurations.json' },
  host_transforms: [{ name: 'fixture.read', input: 'string', output: 'string', slot: 12 }],
  inputs: {
    report_sources: true,
    layers: [{
      origin: 'env_var',
      entries: [{ name: 'DD_MODE', value: 'DEBUG' }],
    }, {
      origin: 'fleet_stable_config',
      entries: [{ name: 'DD_CONTENT', value: 'file', config_id: 'fleet-policy' }],
    }],
  },
}

const first = request(initialization)
assert.equal(first.status, 'pending')
assert.deepEqual(first.requests, [{
  slot: 12,
  key: 'DD_CONTENT',
  origin: 'fleet_stable_config',
  value: { kind: 'string', value: 'file' },
}])

assert.equal(request({ operation: 'resolve', inputs: {} }).status, 'error')
const complete = request({
  operation: 'resume',
  outcomes: [{ status: 'accept', value: { kind: 'string', value: 'contents' } }],
})
assert.equal(complete.status, 'complete')
assert.deepEqual(complete.settings.map(({ resolved }) => [resolved.value.value, resolved.origin]), [
  ['debug', 'env_var'],
  ['contents', 'fleet_stable_config'],
])
assert.equal(complete.sources.length, 4)
assert.deepEqual(complete.sources.map(event => event.config_id), [undefined, undefined, undefined, 'fleet-policy'])
assert.deepEqual(complete.diagnostics, [])

const defaults = request({ operation: 'resolve', inputs: {} })
assert.equal(defaults.status, 'complete')
assert.equal(defaults.settings[0].resolved.value.value, 'info')
assert.equal(defaults.settings[1].resolved, null)
assert.equal(request({ ...initialization, version: 2 }).status, 'error')

assert.equal(request(initialization).status, 'pending')
assert.equal(request({ operation: 'cancel' }).status, 'cancelled')
const missing = request({ operation: 'resume', outcomes: [] })
assert.equal(missing.status, 'error')
assert.match(missing.message, /no pending configuration/)
assert.deepEqual(request({ operation: 'resolve', inputs: {} }), defaults)

assert.equal(request(initialization).status, 'pending')
const invalid = request({ operation: 'resume', outcomes: [] })
assert.equal(invalid.status, 'error')
assert.match(invalid.message, /host returned 0 transform results/)
assert.deepEqual(request({ operation: 'resolve', inputs: {} }), defaults)

const once = request({
  ...initialization,
  operation: 'resolve_once',
  inputs: { ...initialization.inputs, skip_defaults: true },
})
assert.deepEqual(once, first)
const onceComplete = request({
  operation: 'resume',
  outcomes: [{ status: 'accept', value: { kind: 'string', value: 'one-time contents' } }],
})
assert.equal(onceComplete.settings[1].resolved.value.value, 'one-time contents')
assert.equal(onceComplete.sources.length, 2)
assert.equal(onceComplete.sources[1].config_id, 'fleet-policy')
assert.deepEqual(request({ operation: 'resolve', inputs: {} }), defaults)

const typedNumber = 0.00037037037037037035
const radixInteger = BigInt('0x1000000000000081')
const numericManifest = {
  version: '2',
  supportedConfigurations: {
    DD_RADIX: [{ type: 'decimal' }],
    DD_TYPED: [{ type: 'decimal', configurationNames: ['typed'] }],
    DD_TEXT: [{ type: 'string', configurationNames: ['text'] }],
  },
}
const numericInputs = {
  operation: 'initialize',
  manifest: { kind: 'json', value: JSON.stringify(numericManifest) },
  policy: { numbers: 'ecma_script', coerce_code: true },
  inputs: {
    layers: [
      { origin: 'env_var', entries: [] },
      {
        origin: 'code',
        entries: [
          { name: 'typed', value: { kind: 'number', value: typedNumber } },
          { name: 'text', value: { kind: 'number', value: 1571277825824000.25 } },
        ],
      },
    ],
  },
}
for (const text of [
  '0x0',
  '0x20000000000001',
  '0x20000000000003',
  '0x1000000000000081',
  '0b' + radixInteger.toString(2),
  '0o' + radixInteger.toString(8),
  '0x' + 'f'.repeat(256),
  '0b' + '1'.repeat(1025),
]) {
  numericInputs.inputs.layers[0].entries = [{ name: 'DD_RADIX', value: text }]
  const reply = request(numericInputs)
  assert.equal(reply.status, 'complete')
  assert.strictEqual(Number(reply.settings[0].resolved.value.value), Number(text), text)
  assert.strictEqual(reply.settings[1].resolved.value.value, typedNumber)
  assert.strictEqual(reply.settings[2].resolved.value.value, String(1571277825824000.25))
}
