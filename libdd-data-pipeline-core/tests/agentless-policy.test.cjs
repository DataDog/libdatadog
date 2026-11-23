// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

"use strict"

const assert = require("node:assert/strict")
const { copyFileSync, mkdtempSync, readFileSync, rmSync } = require("node:fs")
const { tmpdir } = require("node:os")
const { dirname, join, resolve } = require("node:path")
const test = require("node:test")
const { WASI } = require("node:wasi")

const path = process.env.AGENTLESS_POLICY_WASM ||
  "target/wasm32-wasip1/release/examples/agentless_policy.wasm"
const bytes = readFileSync(path)
const policy = new WebAssembly.Module(bytes)
const hostPath = process.env.AGENTLESS_POLICY_HOST ||
  `${__dirname}/../examples/agentless-policy.cjs`
const loadPolicy = require(resolve(hostPath))

function loadAgentlessPolicy () {
  return loadPolicy(process.env.AGENTLESS_POLICY_HOST ? undefined : bytes)
}

function guest () {
  const wasi = new WASI({ version: "preview1", returnOnExit: true })
  const instance = new WebAssembly.Instance(policy, wasi.getImportObject())
  wasi.initialize(instance)
  return instance.exports
}

function write (exports, bytes) {
  const address = exports.input_buffer(bytes.length) >>> 0
  assert.notEqual(address, 0)
  new Uint8Array(exports.memory.buffer, address, bytes.length).set(bytes)
}

function string (value) {
  const bytes = Buffer.from(value)
  return Buffer.concat([Buffer.from([0xd9, bytes.length]), bytes])
}

function span (id, type, resource, meta = {}) {
  const entries = Object.entries(meta)
  return Buffer.concat([
    Buffer.from([0x86]), string("trace_id"), Buffer.from([id]),
    string("span_id"), Buffer.from([id]), string("name"), string(type),
    string("type"), string(type), string("resource"), string(resource),
    string("meta"), Buffer.from([0x80 | entries.length]),
    ...entries.flatMap(([key, value]) => [string(key), string(value)]),
  ])
}

const batch = Buffer.concat([
  Buffer.from([0x93, 0x91]), span(1, "sql", "SELECT 212345", { "db.type": "postgresql" }),
  Buffer.from([0x91]), span(2, "redis", "SET sample-key sample-value"),
  Buffer.from([0x91]), span(3, "custom", "", { "card.number": "4111111111111111" }),
])

test("packages the exact Wasm bytes for Node bundles without a sidecar", { skip: !process.env.AGENTLESS_POLICY_HOST }, context => {
  const directory = mkdtempSync(join(tmpdir(), "agentless-policy-"))
  context.after(() => rmSync(directory, { recursive: true, force: true }))
  const source = dirname(resolve(hostPath))
  for (const file of ["agentless-policy.cjs", "agentless-policy-bytes.cjs"]) {
    copyFileSync(join(source, file), join(directory, file))
  }

  const bundledBytes = require(join(directory, "agentless-policy-bytes.cjs"))
  assert.deepEqual(Buffer.from(bundledBytes, "base64"), bytes)
  const load = require(join(directory, "agentless-policy.cjs"))
  const result = JSON.parse(load()({}, {}).prepare(batch))
  assert.equal(result.traces[0].spans[0].resource, "SELECT ?")
})

function configure (exports, topLevel = 0) {
  write(exports, Buffer.from("{}"))
  assert.equal(exports.configure(), 1)
  write(exports, Buffer.from('{"hostname":"probe","runtimeID":"synthetic"}'))
  assert.equal(exports.configure_metadata(topLevel), 1)
}

function prepare (exports, clientStats) {
  write(exports, batch)
  const length = exports.prepare(clientStats) >>> 0
  assert.ok(length > 0)
  const address = exports.output_buffer() >>> 0
  assert.notEqual(address, 0)
  return Buffer.from(new Uint8Array(exports.memory.buffer, address, length)).toString()
}

test("prepares each v0.4 trace through the shared policy", () => {
  const exports = guest()
  configure(exports)
  const json = prepare(exports, 0)
  assert.doesNotMatch(json, /212345|sample-key|sample-value|4111111111111111/)
  const { traces } = JSON.parse(json)
  assert.equal(traces.length, 3)
  assert.equal(traces[0].hostname, "probe")
  assert.equal(traces[0].runtimeID, "synthetic")
  const [sql, redis, custom] = traces.map(trace => trace.spans[0])
  assert.equal(sql.resource, "SELECT ?")
  assert.equal(sql.meta["sql.query"], "SELECT ?")
  assert.equal(sql.meta["_dd.compute_stats"], "1")
  assert.equal(redis.resource, "SET")
  assert.equal(custom.resource, "custom")
  assert.equal(custom.meta["card.number"], "?")

  const clientStats = JSON.parse(prepare(exports, 1))
  assert.equal(clientStats.traces[0].spans[0].meta["_dd.compute_stats"], undefined)
})

test("preserves tracer top-level metrics without recomputing them", () => {
  const create = loadAgentlessPolicy()
  const root = span(1, "custom", "operation")
  const input = Buffer.concat([
    Buffer.from([0x91, 0x91, 0x87]), root.subarray(1),
    string("metrics"), Buffer.from([0x81]), string("_dd.top_level"), Buffer.from([1]),
  ])

  for (const clientTopLevel of [false, true]) {
    const host = create({}, {}, clientTopLevel)
    for (const clientStats of [false, true]) {
      const [output] = JSON.parse(host.prepare(input, clientStats)).traces[0].spans
      assert.equal(output.metrics["_dd.top_level"], 1)
      assert.equal(output.metrics._top_level, clientTopLevel || clientStats ? undefined : 1)
      assert.equal(output.meta["_dd.compute_stats"], clientStats ? undefined : "1")
    }
  }
})

test("uses the configured policy without changing the default instance", () => {
  const create = loadAgentlessPolicy()
  const defaults = create({}, { hostname: "default" })
  const configured = create({
    http: { remove_query_string: true, remove_path_digits: true },
    redis: { remove_all_args: true },
    sql: { obfuscation_mode: "obfuscate_and_normalize", keep_positional_parameter: true },
    tag_replace_rules: [{ name: "custom.tag", pattern: "^private:(.*)$", repl: "public:$1" }],
  }, { hostname: "configured" })
  const input = Buffer.concat([
    Buffer.from([0x91, 0x93]),
    span(1, "web", "GET /orders/:id", {
      "http.url": "https://user:sample-password@example.test/orders/741852?secret=sample-query",
      "custom.tag": "private:marker",
    }),
    span(2, "redis", "SET sample-key sample-value", { "redis.raw_command": "SET sample-key sample-value" }),
    span(3, "sql", "SELECT $1, 'sample-literal'", { "db.type": "postgresql" }),
  ])

  const output = configured.prepare(input).toString()
  assert.doesNotMatch(output, /sample-password|sample-query|sample-value|sample-literal|741852|private:marker/)
  const [web, redis, sql] = JSON.parse(output).traces[0].spans
  assert.equal(web.meta["http.url"], "https://example.test/orders/??")
  assert.equal(web.meta["custom.tag"], "public:marker")
  assert.equal(redis.meta["redis.raw_command"], "SET ?")
  assert.equal(sql.resource, "SELECT $1, ?")

  const [defaultWeb, defaultRedis] = JSON.parse(defaults.prepare(input)).traces[0].spans
  assert.equal(defaultWeb.meta["http.url"], "https://example.test/orders/741852?secret=sample-query")
  assert.equal(defaultWeb.meta["custom.tag"], "private:marker")
  assert.equal(defaultRedis.meta["redis.raw_command"], "SET sample-key ?")
})

test("does not expose a previous payload after invalid input or configuration", () => {
  const exports = guest()
  write(exports, batch)
  assert.equal(exports.prepare(0), 0)
  assert.equal(exports.output_buffer(), 0)
  configure(exports)
  prepare(exports, 0)

  const cap = 50 * 1024 * 1024
  assert.notEqual(exports.input_buffer(cap), 0)
  assert.equal(exports.input_buffer(cap + 1), 0)
  assert.equal(exports.prepare(0), 0)
  assert.equal(exports.output_buffer(), 0)

  write(exports, batch.subarray(0, batch.length - 1))
  assert.equal(exports.prepare(0), 0)
  assert.equal(exports.output_buffer(), 0)
  prepare(exports, 0)

  write(exports, Buffer.from("not JSON"))
  assert.equal(exports.configure(), 0)
  write(exports, batch)
  assert.equal(exports.prepare(0), 0)
  assert.equal(exports.output_buffer(), 0)
  configure(exports)
  prepare(exports, 0)
})

test("rejects invalid host configuration and input without reusing a previous payload", () => {
  const create = loadAgentlessPolicy()
  assert.throws(() => create({ redis: { remove_all_args: "true" } }, {}), {
    message: "Invalid agentless policy configuration",
  })

  const host = create({}, { hostname: "first" })
  assert.ok(host.prepare(batch))
  assert.equal(host.prepare(new Uint8Array(0)), undefined)
  assert.equal(host.prepare(new Uint8Array(50 * 1024 * 1024 + 1)), undefined)
  assert.throws(() => host.configureMetadata({ hostname: 42 }), {
    message: "Invalid agentless policy metadata",
  })
  assert.equal(host.prepare(batch), undefined)
  host.configureMetadata({ hostname: "recovered" })
  assert.equal(JSON.parse(host.prepare(batch)).traces[0].hostname, "recovered")
})

test("the Node host copies results before it reuses guest memory", () => {
  const create = loadAgentlessPolicy()
  const first = create({}, { hostname: "first" })
  const second = create({}, { hostname: "second" })
  const original = first.prepare(batch)
  assert.equal(first.prepare(batch.subarray(0, batch.length - 1)), undefined)
  first.configureMetadata({ hostname: "updated" })
  const updated = first.prepare(batch)
  assert.equal(JSON.parse(updated).traces[0].hostname, "updated")
  assert.equal(JSON.parse(second.prepare(batch)).traces[0].hostname, "second")
  assert.equal(JSON.parse(original).traces[0].hostname, "first")
  assert.doesNotMatch(updated.toString(), /212345|4111111111111111/)
})

test("keeps WASI imports isolated and rejects out-of-range ABI values", context => {
  const Instance = WebAssembly.Instance
  let inputBuffer
  let guestExports
  let wasiImports
  let suppressOutputAddress = false
  context.mock.method(WebAssembly, "Instance", function (module, imports) {
    wasiImports = imports.wasi_snapshot_preview1
    guestExports = new Instance(module, imports).exports
    inputBuffer = context.mock.fn(guestExports.input_buffer)
    return {
      exports: {
        ...guestExports,
        input_buffer: inputBuffer,
        output_buffer: () => suppressOutputAddress ? 0 : guestExports.output_buffer(),
      },
    }
  })

  const host = loadAgentlessPolicy()({}, { hostname: "boundary" })
  assert.ok(host.prepare(batch))
  const oversize = new Uint8Array(0)
  Object.defineProperty(oversize, "byteLength", { value: 0x1_0000_0000 })
  const callsBefore = inputBuffer.mock.callCount()
  assert.equal(host.prepare(oversize), undefined)
  assert.equal(inputBuffer.mock.callCount(), callsBefore)

  const address = guestExports.input_buffer(8) >>> 0
  assert.notEqual(address, 0)
  const environment = new Uint8Array(guestExports.memory.buffer, address, 8).fill(0xff)
  assert.equal(wasiImports.environ_get(address, address + 4), 0)
  assert.ok(environment.every(byte => byte === 0xff))
  assert.equal(wasiImports.environ_sizes_get(address, address + 4), 0)
  assert.ok(environment.every(byte => byte === 0))
  assert.throws(() => wasiImports.fd_write(), { message: "Agentless policy reported a fatal error" })
  assert.throws(() => wasiImports.proc_exit(42), { message: "Agentless policy exited with status 42" })

  suppressOutputAddress = true
  assert.throws(() => host.prepare(batch), { message: "Missing agentless policy output" })
})
