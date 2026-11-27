// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import { readFileSync } from "node:fs"
import test from "node:test"
import { WASI } from "node:wasi"

import { agentlessFixture } from "./agentless-fixture.mjs"

const schema = JSON.parse(readFileSync(new URL("../abi.json", import.meta.url), "utf8"))
const artifact = process.env.DD_RC_TEST_WASM ?? new URL(
  "../../target/wasm32-wasip1/debug/libdd_remote_config_wasm.wasm", import.meta.url
)
const nativeArtifact = process.env.DD_RC_TEST_NATIVE
let compiled
const path = "datadog/2/APM_TRACING/wasm/config"
const options = {
  target: { service: "test", env: "test", appVersion: "1" },
  identity: { clientId: "wasm-client", runtimeId: "wasm-runtime" },
  interest: { products: ["APM_TRACING"], capabilities: ["APM_TRACING_SAMPLE_RATE"] },
  language: "nodejs",
  tracerVersion: "1",
  url: "http://127.0.0.1:8126",
  timeoutMs: 2000
}

class Reactor {
  #exports
  #native

  /**
   * @param {{ exports: Record<string, (...args: number[]) => number>,
   *   read: (buffer: number, length: number) => Buffer,
   *   write: (buffer: number, bytes: Uint8Array) => void }} [native]
   */
  constructor (native) {
    if (native !== undefined) {
      this.#native = native
      this.#exports = native.exports
      return
    }
    compiled ??= new WebAssembly.Module(readFileSync(artifact))
    const wasi = new WASI({ version: "preview1", args: [], env: {}, preopens: {}, returnOnExit: true })
    const instance = new WebAssembly.Instance(compiled, wasi.getImportObject())
    wasi.initialize(instance)
    this.#exports = instance.exports
  }

  get exports () {
    return this.#exports
  }

  /** @param {string} name @param {...number} args */
  call (name, ...args) {
    const result = this.#exports[name](...args)
    if (result === -1) {
      const error = this.#exports.rc_error()
      throw new Error(error === -1 ? "cannot read reactor error" : this.take(error).toString())
    }
    return result
  }

  /** @param {Uint8Array} bytes */
  input (bytes) {
    const buffer = this.call("rc_alloc", bytes.length)
    if (this.#native === undefined) {
      const offset = this.call("rc_buffer_ptr", buffer) >>> 0
      new Uint8Array(this.#exports.memory.buffer, offset, bytes.length).set(bytes)
    } else {
      this.#native.write(buffer, bytes)
    }
    return buffer
  }

  /** @param {object} value @param {(handle: number) => number} operation */
  withJson (value, operation) {
    const buffer = this.input(Buffer.from(JSON.stringify(value)))
    try {
      return operation(buffer)
    } finally {
      this.call("rc_free", buffer)
    }
  }

  /** @param {number} buffer */
  take (buffer) {
    if (buffer === 0) {
      return Buffer.alloc(0)
    }
    try {
      if (this.#native !== undefined) {
        return this.#native.read(buffer, this.call("rc_buffer_len", buffer))
      }
      const offset = this.call("rc_buffer_ptr", buffer) >>> 0
      const length = this.call("rc_buffer_len", buffer)
      return Buffer.from(new Uint8Array(this.#exports.memory.buffer, offset, length))
    } finally {
      this.call("rc_free", buffer)
    }
  }

  /** @param {string} name @param {...number} args */
  json (name, ...args) {
    const buffer = this.call(name, ...args)
    return buffer === 0 ? undefined : JSON.parse(this.take(buffer).toString())
  }

  open (clientOptions = options) {
    return this.withJson(clientOptions, buffer => this.call("rc_open", buffer))
  }

  /** @param {number} client @param {string} [path] */
  start (client, path = "/v0.7/config") {
    this.call("rc_poll", client)
    const request = this.json("rc_event", client)
    const timer = this.json("rc_event", client)
    assert.equal(request.kind, "http")
    assert.equal(request.method, "POST")
    assert.equal(new URL(request.url).pathname, path)
    assert.equal(timer.kind, "sleep")
    assert.equal(timer.delayMs, options.timeoutMs)
    assert.equal(this.call("rc_event", client), 0)
    assert.equal(this.call("rc_result", client), 0)
    return { request: JSON.parse(this.take(request.body).toString()), httpId: request.id, timerId: timer.id }
  }

  /** @param {number} client @param {object} response @param {number} [status] */
  poll (client, response, status = 200) {
    const { request, httpId, timerId } = this.start(client)
    const body = this.input(Buffer.isBuffer(response) ? response : Buffer.from(JSON.stringify(response)))
    this.withJson({ status }, metadata => this.call("rc_http_succeed", client, httpId, metadata, body))
    assert.equal(this.#exports.rc_buffer_len(body), -1)
    assert.deepEqual(this.json("rc_event", client), { kind: "cancel", id: timerId })
    return { request, ...this.json("rc_result", client) }
  }

  /** @param {number} view @param {number} [previous] */
  file (view, previous = 0) {
    return this.take(this.call("rc_view_file", view, 0, previous))
  }
}

/**
 * @param {bigint} version
 * @param {Uint8Array} [contents]
 * @param {string} [backendState]
 * @param {number} [length]
 */
function assignment (version, contents, backendState = "test-state", length = contents?.length) {
  const targets = contents === undefined ? {} : {
    [path]: { custom: { v: "FILE_VERSION" }, hashes: {
      sha256: createHash("sha256").update(contents).digest("hex")
    }, length }
  }
  const signedTargets = JSON.stringify({ signed: {
    custom: { opaque_backend_state: backendState }, targets, version: 1
  } }).replace("\"FILE_VERSION\"", version.toString())
  return {
    roots: [],
    targets: Buffer.from(signedTargets).toString("base64"),
    target_files: contents === undefined ? [] : [{ path, raw: Buffer.from(contents).toString("base64") }],
    client_configs: contents === undefined ? [] : [path]
  }
}

test("exports the generated scalar ABI and accepts only bounded buffers", () => {
  const reactor = new Reactor()
  assert.equal(reactor.call("rc_version"), schema.version)
  const exports = Object.keys(reactor.exports).filter(name => name.startsWith("rc_"))
  assert.deepEqual(exports.sort(), Object.keys(schema.exports).sort())
  for (const [name, args] of Object.entries(schema.exports)) {
    assert.equal(reactor.exports[name].length, args.length, name)
  }
  for (const dependency of WebAssembly.Module.imports(compiled)) {
    assert.equal(dependency.module, "wasi_snapshot_preview1")
  }
  assert.throws(() => reactor.call("rc_alloc", 16 * 1024 * 1024 + 1), /buffer exceeds/)
  const limit = reactor.call("rc_alloc", 16 * 1024 * 1024)
  reactor.call("rc_free", limit)
  assert.throws(() => reactor.call("rc_free", limit), /invalid or freed/)
})

testBackends("Agent session appends the config route to its HTTP base path", reactor => {
  const client = reactor.open({ ...options, url: "http://127.0.0.1:8126/prefix%2Ftenant/" })
  reactor.start(client, "/prefix%2Ftenant/v0.7/config")
  reactor.call("rc_close", client)
})

testBackends("keeps an HTTP event queued when its output does not fit", reactor => {
  const client = reactor.open()
  reactor.call("rc_poll", client)
  const pinned = Array.from({ length: 255 }, () => reactor.call("rc_alloc", 1))
  assert.throws(() => reactor.call("rc_event", client), /buffer count exhausted/)
  reactor.call("rc_free", pinned.pop())
  const request = reactor.json("rc_event", client)
  assert.equal(request.kind, "http")
  assert.equal(JSON.parse(reactor.take(request.body).toString()).client.id, options.identity.clientId)
  const timer = reactor.json("rc_event", client)
  assert.equal(timer.kind, "sleep")
  assert.equal(reactor.call("rc_event", client), 0)
  for (const buffer of pinned) reactor.call("rc_free", buffer)
  reactor.withJson({ status: 204 }, metadata => reactor.call("rc_http_succeed", client, request.id, metadata, 0))
  assert.deepEqual(reactor.json("rc_event", client), { kind: "cancel", id: timer.id })
  assert.equal(reactor.json("rc_result", client).error, undefined)
  reactor.call("rc_close", client)
})

testBackends("keeps a poll result queued when its output does not fit", reactor => {
  const client = reactor.open()
  const initial = reactor.poll(client, assignment(1n, Buffer.from("initial")))
  reactor.call("rc_view_release", initial.batch)
  const views = Array.from({ length: 31 }, () => reactor.call("rc_snapshot", client))
  const { httpId, timerId } = reactor.start(client)
  const response = assignment(2n, Buffer.from("changed"))
  const body = reactor.input(Buffer.from(JSON.stringify(response)))
  reactor.withJson({ status: 200 }, metadata => reactor.call("rc_http_succeed", client, httpId, metadata, body))
  assert.deepEqual(reactor.json("rc_event", client), { kind: "cancel", id: timerId })
  const pinned = Array.from({ length: 256 }, () => reactor.call("rc_alloc", 1))
  assert.throws(() => reactor.call("rc_result", client))
  reactor.call("rc_free", pinned.pop())
  const result = reactor.json("rc_result", client)
  const descriptor = reactor.json("rc_view_describe", result.batch)
  assert.equal(descriptor.records[0].file.version, "2")
  assert.equal(reactor.call("rc_result", client), 0)
  for (const buffer of pinned) reactor.call("rc_free", buffer)
  for (const view of views) reactor.call("rc_view_release", view)
  reactor.call("rc_view_release", result.batch)
  reactor.call("rc_close", client)
})

test("maps numeric capability indices to the same request bits as names", () => {
  const reactor = new Reactor()
  const client = reactor.open({
    ...options,
    interest: { products: ["APM_TRACING"], capabilityIndices: [46, 12, 12] }
  })
  const numeric = [0x40, 0, 0, 0, 0x10, 0]
  assert.deepEqual(reactor.poll(client, {}).request.client.capabilities, numeric)

  const setInterest = interest => reactor.withJson({ products: ["APM_TRACING"], ...interest },
    handle => reactor.call("rc_set_interest", client, handle))
  assert.throws(() => setInterest({ capabilityIndices: [0] }), /unsupported capability index 0/)
  assert.throws(() => setInterest({ capabilityIndices: [255] }), /unsupported capability index 255/)
  assert.throws(() => setInterest({ capabilityIndices: [256] }), /u8/)
  assert.throws(() => setInterest({ capabilityIndices: new Array(257).fill(12) }), /too many capability indices/)
  assert.throws(() => setInterest({ capabilityIndices: [12], capabilities: ["APM_TRACING_SAMPLE_RATE"] }), /not both/)
  assert.deepEqual(reactor.poll(client, {}).request.client.capabilities, numeric)

  setInterest({ capabilities: ["APM_TRACING_SAMPLE_RATE", "FFE_FLAG_CONFIGURATION_RULES"] })
  assert.deepEqual(reactor.poll(client, {}).request.client.capabilities, numeric)
  setInterest({ capabilityIndices: [] })
  assert.deepEqual(reactor.poll(client, {}).request.client.capabilities, [])
  reactor.call("rc_close", client)
})

testBackends("preserves the Agent URL path prefix", reactor => {
  const client = reactor.open({ ...options, url: `${options.url}/prefix%2Ftenant/?unused=1` })
  reactor.call("rc_poll", client)
  const request = reactor.json("rc_event", client)
  assert.equal(request.kind, "http")
  assert.equal(request.url, `${options.url}/prefix%2Ftenant/v0.7/config`)
  reactor.call("rc_free", request.body)
  reactor.call("rc_close", client)
})

testBackends("refreshes process tags and retains outstanding receipts", reactor => {
  const client = reactor.open({
    ...options, target: { ...options.target, processTags: ["svc.auto:initial"] }
  })
  /** @param {{ processTags?: string[], runtimeId?: string }} identity */
  const setIdentity = identity => reactor.withJson({ ...options.identity, ...identity },
    input => reactor.call("rc_set_identity", client, input))

  setIdentity({ processTags: ["svc.auto:before"] })
  setIdentity({ runtimeId: "updated-runtime" })
  const original = reactor.poll(client, assignment(1n, Buffer.from("{}")))
  assert.deepEqual(original.request.client.client_tracer.process_tags, ["svc.auto:before"])
  const receipt = reactor.call("rc_view_file", original.batch, 0, 0)
  reactor.call("rc_view_release", original.batch)

  setIdentity({ processTags: ["svc.user:true"] })
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 1)
  reactor.call("rc_free", receipt)
  const refreshed = reactor.poll(client, {}, 204)
  assert.deepEqual(refreshed.request.client.client_tracer.process_tags, ["svc.user:true"])
  assert.equal(refreshed.request.client.state.config_states[0].apply_state, 2)
  assert.equal(refreshed.batch, 0)

  setIdentity({})
  const preserved = reactor.poll(client, {}, 204)
  assert.deepEqual(preserved.request.client.client_tracer.process_tags, ["svc.user:true"])
  setIdentity({ processTags: [] })
  const cleared = reactor.poll(client, {}, 204)
  assert.deepEqual(cleared.request.client.client_tracer.process_tags ?? [], [])
  reactor.call("rc_close", client)
})

testBackends("uses identity process tags for open and retarget", reactor => {
  const target = { ...options.target, processTags: ["svc.auto:target"] }
  const identity = { ...options.identity, processTags: ["svc.auto:identity"] }
  const client = reactor.open({ ...options, target, identity })
  const initial = reactor.poll(client, {})
  assert.deepEqual(initial.request.client.client_tracer.process_tags, ["svc.auto:identity"])

  reactor.withJson({ target, identity: { ...identity, processTags: ["svc.user:true"] } },
    input => reactor.call("rc_retarget", client, input))
  const retargeted = reactor.poll(client, {})
  assert.deepEqual(retargeted.request.client.client_tracer.process_tags, ["svc.user:true"])
  reactor.call("rc_close", client)
})

testBackends("distinguishes readiness from no change and retains revisions", reactor => {
  const client = reactor.open()
  assert.equal(reactor.call("rc_snapshot", client), 0)
  const blank = reactor.poll(client, {})
  assert.equal(blank.ready, false)
  assert.equal(blank.batch, 0)

  const empty = reactor.poll(client, assignment(0n))
  assert.equal(empty.ready, true)
  assert.notEqual(empty.batch, 0)
  assert.deepEqual(reactor.json("rc_view_describe", empty.batch).records, [])
  reactor.call("rc_view_release", empty.batch)

  const firstBytes = Buffer.from([0, 255, 128, 1])
  const original = reactor.poll(client, assignment(1n, firstBytes))
  const description = reactor.json("rc_view_describe", original.batch)
  assert.equal(description.records[0].file.path, path)
  assert.equal(description.records[0].file.product, "APM_TRACING")
  assert.equal(description.records[0].kind, "upsert")
  const originalSnapshot = reactor.call("rc_view_snapshot", original.batch)

  const unchanged = reactor.poll(client, {})
  assert.equal(unchanged.request.client.state.config_states[0].apply_state, 1)
  assert.equal(unchanged.batch, 0)
  reactor.exports.memory?.grow(1)

  const secondBytes = Buffer.from([0, 2, 255, 3])
  const replacement = reactor.poll(client, assignment(9007199254740993n, secondBytes))
  const record = reactor.json("rc_view_describe", replacement.batch).records[0]
  assert.equal(record.file.version, "9007199254740993")
  assert.equal(record.previous.version, "1")
  assert.deepEqual(reactor.file(originalSnapshot), firstBytes)
  assert.deepEqual(reactor.file(replacement.batch, 1), firstBytes)
  assert.deepEqual(reactor.file(replacement.batch), secondBytes)
  const originalReceipt = reactor.call("rc_view_file", original.batch, 0, 0)
  const replacementReceipt = reactor.call("rc_view_file", replacement.batch, 0, 0)
  reactor.call("rc_view_release", original.batch)
  reactor.call("rc_view_release", replacement.batch)
  assert.equal(reactor.call("rc_apply", client, originalReceipt, 2, 0), 0)

  const otherClient = reactor.open()
  assert.equal(reactor.call("rc_apply", otherClient, replacementReceipt, 2, 0), 0)
  reactor.call("rc_close", otherClient)
  assert.equal(reactor.call("rc_apply", client, replacementReceipt, 2, 0), 1)

  const removed = reactor.poll(client, assignment(0n))
  assert.equal(removed.request.client.state.config_states[0].apply_state, 2)
  assert.equal(reactor.json("rc_view_describe", removed.batch).records[0].kind, "remove")
  const removalReceipt = reactor.call("rc_view_file", removed.batch, 0, 0)
  assert.equal(reactor.call("rc_apply", client, removalReceipt, 2, 0), 0)
  assert.equal(reactor.call("rc_apply", client, replacementReceipt, 2, 0), 0)
  reactor.call("rc_close", client)
  assert.equal(reactor.call("rc_apply", client, replacementReceipt, 2, 0), 0)
  for (const receipt of [originalReceipt, replacementReceipt, removalReceipt]) {
    reactor.call("rc_free", receipt)
  }
  for (const retained of [originalSnapshot, removed.batch]) {
    reactor.call("rc_view_release", retained)
  }
})

testBackends("completes deferred raw receipts without holding file or view handles", reactor => {
  const client = reactor.open()
  const other = reactor.open()
  const first = assignment(1n, Buffer.from("first"))
  const second = assignment(2n, Buffer.from("second"))
  const observe = (owner, response) => {
    const { batch } = reactor.poll(owner, response)
    const description = reactor.json("rc_view_describe", batch)
    reactor.call("rc_view_release", batch)
    assert.equal(description.receiptOwner, owner)
    return description
  }
  const complete = ({ receiptOwner, generation }, state = 2, owner = client, extras = []) =>
    reactor.withJson({ receiptOwner, outcomes: [{ path, generation, state, message: "rejected ✕" }, ...extras] },
      input => reactor.call("rc_apply_receipts", owner, input))
  const status = owner => reactor.poll(owner, {}).request.client.state.config_states.find(item => item.id === "wasm")

  const original = observe(client, first)
  const independent = observe(other, first)
  assert.equal(original.generation, independent.generation)
  assert.equal(complete(original, 2, other), 0)
  assert.equal(status(other).apply_state, 1)
  assert.equal(complete({ ...original, generation: "9007199254740993" }), 0)
  assert.throws(() => complete(original, 2, client, [{ path, generation: original.generation, state: 4 }]),
    /invalid apply state/)
  assert.equal(status(client).apply_state, 1)

  const neighbor = "datadog/2/APM_TRACING/neighbor/config"
  const combined = structuredClone(first)
  const metadata = JSON.parse(Buffer.from(combined.targets, "base64"))
  metadata.signed.targets[neighbor] = metadata.signed.targets[path]
  combined.targets = Buffer.from(JSON.stringify(metadata)).toString("base64")
  combined.client_configs.push(neighbor)
  combined.target_files.push({ ...combined.target_files[0], path: neighbor })
  observe(client, combined)
  assert.equal(complete(original, 3), 1)
  assert.equal(status(client).apply_error, "rejected ✕")

  const replacement = observe(client, second)
  assert.equal(complete(original), 0)
  assert.equal(status(client).apply_state, 1)
  const watch = reactor.withJson({ delivery: "latestState", products: ["APM_TRACING"] },
    input => reactor.call("rc_watch_open", client, input))
  assert.equal(complete(replacement), 0)
  reactor.call("rc_watch_close", client, watch)
  assert.equal(complete(replacement), 1)
  assert.equal(status(client).apply_state, 2)

  observe(client, assignment(0n))
  assert.equal(complete(replacement), 0)
  const restored = observe(client, second)
  assert.equal(complete(replacement), 0)
  assert.equal(complete(restored), 1)
  reactor.withJson({ target: { service: "another", env: "test", appVersion: "1" }, identity: options.identity },
    input => reactor.call("rc_retarget", client, input))
  assert.equal(complete(restored), 0)
  const retargeted = observe(client, second)
  assert.equal(complete(restored), 0)
  assert.equal(complete(retargeted), 1)
  reactor.call("rc_close", client)
  assert.equal(complete(retargeted), 0)
  reactor.call("rc_close", other)
})

testBackends("completes projected decisions together without opening payload handles", reactor => {
  const client = reactor.open()
  const other = reactor.open()
  const files = ["eligible", "ignored", "rejected"]
  const assign = (version, names = files) => {
    const response = assignment(0n)
    const metadata = JSON.parse(Buffer.from(response.targets, "base64"))
    for (const name of names) {
      const path = `datadog/2/APM_TRACING/${name}/config`
      const bytes = Buffer.from(`${name}-${version}`)
      metadata.signed.targets[path] = {
        custom: { v: version },
        hashes: { sha256: createHash("sha256").update(bytes).digest("hex") },
        length: bytes.length
      }
      response.target_files.push({ path, raw: bytes.toString("base64") })
      response.client_configs.push(path)
    }
    response.targets = Buffer.from(JSON.stringify(metadata)).toString("base64")
    return reactor.poll(client, response).batch
  }
  const diagnostic = error => {
    const text = Buffer.from(error)
    const length = Buffer.alloc(4)
    length.writeUInt32LE(text.length)
    return [length, text]
  }
  const encode = (outcome, files) => {
    const bytes = [Buffer.from([{ applied: 0, retained: 1, failed: 2 }[outcome.kind]])]
    if (outcome.kind === "failed") bytes.push(...diagnostic(outcome.error))
    for (const { index, decision } of files) {
      const entry = Buffer.alloc(5)
      entry.writeUInt32LE(index)
      entry[4] = { eligible: 0, ignored: 1, rejected: 2 }[decision.kind]
      bytes.push(entry)
      if (decision.kind === "rejected") bytes.push(...diagnostic(decision.error))
    }
    return Buffer.concat(bytes)
  }
  const reportBytes = (batch, bytes, owner = client) => {
    const handle = reactor.input(bytes)
    try {
      return reactor.call("rc_apply_plan", owner, batch, handle)
    } finally {
      reactor.call("rc_free", handle)
    }
  }
  const report = (batch, outcome, files, owner = client) => reportBytes(batch, encode(outcome, files), owner)
  const statuses = () => Object.fromEntries(reactor.poll(client, {}).request.client.state.config_states.map(
    ({ id, apply_state, apply_error }) => [id, [apply_state, apply_error ?? ""]]))
  const initial = assign(1)
  const decisions = reactor.json("rc_view_describe", initial).records.map(({ file }, index) => ({
    index,
    decision: file.configId === "rejected" ? { kind: "rejected", error: "invalid setting ✕" } : { kind: file.configId }
  }))
  const applied = { kind: "applied" }
  assert.equal(report(initial, applied, decisions, other), 0)
  assert.throws(() => report(initial, applied, [...decisions, decisions.at(-1)]), /unique and increasing/)
  assert.throws(() => report(initial, applied, [...decisions, { index: 3, decision: { kind: "eligible" } }]),
    /invalid view index/)
  assert.throws(() => reportBytes(initial, Buffer.alloc(0)), /missing plan outcome/)
  assert.throws(() => reportBytes(initial, Buffer.from([3])), /invalid plan outcome/)
  assert.throws(() => reportBytes(initial, Buffer.from([2, 255, 255, 255, 255])), /truncated plan diagnostic/)
  assert.throws(() => reportBytes(initial, Buffer.from([0, 0, 0, 0])), /truncated plan word/)
  assert.throws(() => reportBytes(initial, Buffer.from([0, 0, 0, 0, 0])), /missing plan decision/)
  assert.throws(() => reportBytes(initial, Buffer.from([0, 0, 0, 0, 0, 3])), /invalid plan decision/)
  assert.throws(() => reportBytes(initial, Buffer.from([0, 0, 0, 0, 0, 2, 1, 0, 0, 0, 255])), /invalid UTF-8/)
  assert.throws(() => reportBytes(initial, encode(applied, decisions).subarray(0, -1)), /truncated plan/)
  assert.throws(() => report(initial, { kind: "failed", error: "x".repeat(6 * 1024 * 1024) },
    decisions.map(({ index }) => ({ index, decision: { kind: "eligible" } }))), /diagnostic budget/)
  assert.deepEqual(statuses(), { eligible: [1, ""], ignored: [1, ""], rejected: [1, ""] })

  assert.equal(report(initial, { kind: "failed", error: "installer refused" }, decisions), 3)
  assert.deepEqual(statuses(), {
    eligible: [3, "installer refused"], ignored: [2, ""], rejected: [3, "invalid setting ✕"]
  })
  const replacement = assign(2, ["eligible"])
  const records = reactor.json("rc_view_describe", replacement).records
  const index = records.findIndex(record => record.kind === "upsert")
  const decisionsForReplacement = [{ index, decision: { kind: "eligible" } }]
  assert.equal(report(initial, applied, decisions), 0)
  const removed = records.findIndex(record => record.kind === "remove")
  assert.throws(() => report(replacement, applied, [{ index: removed, decision: { kind: "eligible" } }]),
    /only upsert records/)
  const snapshot = reactor.call("rc_view_snapshot", replacement)
  assert.throws(() => report(snapshot, applied, [{ index: 0, decision: { kind: "eligible" } }]), /only upsert records/)
  assert.deepEqual(statuses(), { eligible: [1, ""] })
  assert.equal(report(replacement, { kind: "retained" }, decisionsForReplacement), 1)
  assert.deepEqual(statuses(), { eligible: [2, ""] })
  const last = assign(3, ["eligible"])
  assert.equal(report(last, applied, [{ index: 0, decision: { kind: "eligible" } }]), 1)
  assert.deepEqual(statuses(), { eligible: [2, ""] })
  for (const view of [initial, replacement, snapshot, last]) {
    reactor.call("rc_view_release", view)
  }
  reactor.call("rc_close", other)
  reactor.call("rc_close", client)
})

function assertAgentIntegrity(reactor) {
  const client = reactor.open()
  const initialBytes = Buffer.from("initial")
  const initial = reactor.poll(client, assignment(1n, initialBytes))
  assert.notEqual(initial.batch, 0)
  reactor.call("rc_view_release", initial.batch)

  const changed = Buffer.from("verified-response")
  const wrongHash = assignment(2n, changed)
  wrongHash.target_files[0].raw = Buffer.alloc(changed.length).toString("base64")
  const invalid = [
    { reply: wrongHash, message: /Computed hash/ },
    { reply: assignment(2n, changed, undefined, changed.length - 1), message: /Decoded length/ },
    { reply: assignment(2n, changed, undefined, changed.length + 1), message: /Decoded length/ }
  ]
  for (const { reply, message } of invalid) {
    const failed = reactor.poll(client, reply)
    assert.equal(failed.batch, 0, message.source)
    assert.equal(failed.ready, true)
    const retained = reactor.call("rc_snapshot", client)
    assert.deepEqual(reactor.file(retained), initialBytes)
    reactor.call("rc_view_release", retained)

    for (const status of [404, 204]) {
      const report = reactor.poll(client, Buffer.alloc(0), status)
      assert.equal(report.error, undefined)
      assert.equal(report.request.client.state.has_error, true, `status ${status}`)
      assert.match(report.request.client.state.error, message)
      assert.equal(report.request.client.state.config_states[0].version, 1)
    }
    assert.equal(reactor.poll(client, {}).request.client.state.has_error, false)
  }

  const verified = reactor.poll(client, assignment(2n, changed))
  assert.notEqual(verified.batch, 0)
  assert.deepEqual(reactor.file(verified.batch), changed)
  reactor.call("rc_view_release", verified.batch)
  reactor.call("rc_close", client)
}

testBackends("marks only nonempty batches whose changes are covered by live watches", reactor => {
  const client = reactor.open()
  const initial = reactor.poll(client, assignment(1n, Buffer.from("initial")))
  assert.equal(initial.batchFullyWatched, undefined)
  reactor.call("rc_view_release", initial.batch)

  const watch = reactor.withJson({ delivery: "latestState", products: ["APM_TRACING"] },
    buffer => reactor.call("rc_watch_open", client, buffer))
  const watched = reactor.poll(client, assignment(2n, Buffer.from("watched")))
  assert.equal(watched.batchFullyWatched, true)
  assert.deepEqual(reactor.file(watched.batch), Buffer.from("watched"))
  reactor.call("rc_view_release", watched.batch)

  const unchanged = reactor.poll(client, {}, 204)
  assert.equal(unchanged.batch, 0)
  assert.equal(unchanged.batchFullyWatched, undefined)

  assert.ok(reactor.call("rc_watch_next", client, watch) > 0)
  assert.equal(reactor.call("rc_watch_close", client, watch), 0)
  const closing = reactor.poll(client, assignment(3n, Buffer.from("closing")))
  assert.equal(closing.batchFullyWatched, undefined)
  assert.deepEqual(reactor.file(closing.batch), Buffer.from("closing"))
  reactor.call("rc_view_release", closing.batch)
  reactor.call("rc_close", client)
})

testBackends("drives a typed watch with scoped views and no low-level completion authority", reactor => {
  const client = reactor.open({
    ...options,
    interest: { ...options.interest, products: ["APM_TRACING", "LIVE_DEBUGGING"] }
  })
  const settings = { delivery: "latestState", products: ["APM_TRACING"] }
  const open = () => reactor.withJson(settings, buffer => reactor.call("rc_watch_open", client, buffer))
  const watch = open()
  assert.throws(open, /already has a watch owner/)
  assert.equal(reactor.call("rc_watch_replay", client, watch), 0)
  assert.equal(reactor.call("rc_watch_next", client, watch), 0)
  const cancelled = reactor.start(client)
  assert.throws(() => reactor.call("rc_watch_replay", client, watch), /poll is in progress/)
  reactor.call("rc_cancel", client)
  const cancelledEvents = [reactor.json("rc_event", client), reactor.json("rc_event", client)]
  assert.deepEqual(new Set(cancelledEvents), new Set([
    { kind: "cancel", id: cancelled.httpId }, { kind: "cancel", id: cancelled.timerId }
  ]))
  assert.match(reactor.json("rc_result", client).error, /poll cancelled/)
  assert.equal(reactor.poll(client, {}, 204).ready, false)
  assert.equal(reactor.call("rc_watch_next", client, watch), 0)

  const response = assignment(1n, Buffer.from("value"))
  const metadata = JSON.parse(Buffer.from(response.targets, "base64"))
  const sibling = "datadog/2/LIVE_DEBUGGING/sibling/config"
  metadata.signed.targets[sibling] = metadata.signed.targets[path]
  response.targets = Buffer.from(JSON.stringify(metadata)).toString("base64")
  response.target_files.push({ ...response.target_files[0], path: sibling })
  response.client_configs.push(sibling)
  const { batch, batchFullyWatched } = reactor.poll(client, response)
  assert.equal(batchFullyWatched, undefined)
  const pinned = Array.from({ length: 31 }, () => reactor.call("rc_snapshot", client))
  const idleClient = reactor.open()
  const idleWatch = reactor.withJson(settings, buffer => reactor.call("rc_watch_open", idleClient, buffer))
  assert.equal(reactor.call("rc_watch_next", idleClient, idleWatch), 0)
  assert.equal(reactor.poll(idleClient, {}, 204).batch, 0)
  assert.equal(reactor.call("rc_watch_next", idleClient, idleWatch), 0)
  reactor.call("rc_close", idleClient)
  assert.throws(() => reactor.call("rc_watch_next", client, watch), /view budget exhausted/)
  for (const snapshot of pinned) reactor.call("rc_view_release", snapshot)
  const update = reactor.call("rc_watch_next", client, watch)
  const descriptor = reactor.json("rc_view_describe", update)
  assert.equal(descriptor.reason, "initial")
  assert.deepEqual(descriptor.records.map(record => record.file.path), [path])
  const snapshot = reactor.call("rc_view_snapshot", update)
  assert.deepEqual(reactor.json("rc_view_describe", snapshot).records.map(record => record.file.path), [path])
  const receipt = reactor.call("rc_view_file", snapshot, 0, 0)
  const next = () => reactor.call("rc_watch_next", client, watch)
  const finish = (view, bytes, owner = watch) => {
    const buffer = reactor.input(Buffer.from(bytes))
    try {
      return reactor.call("rc_watch_finish", client, owner, view, buffer)
    } finally {
      reactor.call("rc_free", buffer)
    }
  }
  assert.equal(next(), 0)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 0)
  assert.throws(() => reactor.call("rc_view_release", update), /finish a watch update/)
  assert.throws(() => finish(update, [0], watch + 1), /does not belong/)
  assert.throws(() => finish(update, [0]), /required for each upsert/)
  assert.equal(reactor.poll(client, {}).request.client.state.config_states.find(config => config.id === "wasm").apply_state, 1)
  assert.equal(finish(update, [2, 4, 0, 0, 0, ...Buffer.from("busy"), 0, 0, 0, 0, 0]), 1)
  assert.equal(next(), 0)
  const status = reactor.poll(client, {}).request.client.state.config_states.find(config => config.id === "wasm")
  assert.equal(status.apply_state, 3)
  assert.equal(status.apply_error, "busy")
  const retry = next()
  assert.equal(reactor.json("rc_view_describe", retry).reason, "retry")
  assert.throws(() => finish(update, [0]), /invalid or released view/)
  assert.equal(finish(retry, [0, 0, 0, 0, 0, 0]), 1)
  assert.equal(reactor.call("rc_watch_replay", client, watch), 1)
  const replay = next()
  assert.equal(reactor.json("rc_view_describe", replay).reason, "replay")
  assert.equal(reactor.call("rc_watch_close", client, watch), 0)
  assert.throws(open, /already has a watch owner/)
  assert.equal(finish(replay, [1]), 0)
  const late = open()
  assert.ok(late > watch)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 0)
  reactor.call("rc_free", receipt)
  reactor.call("rc_view_release", snapshot)
  reactor.call("rc_view_release", batch)
  reactor.call("rc_close", client)
})

testBackends("delivers watch upserts in response order while retaining snapshot positions", reactor => {
  const client = reactor.open()
  const watch = reactor.withJson({ delivery: "latestState", products: ["APM_TRACING"] },
    buffer => reactor.call("rc_watch_open", client, buffer))
  const sibling = "datadog/2/APM_TRACING/sibling/config"

  /** @param {bigint} version @param {number} siblingVersion @param {string[]} clientConfigs */
  const submit = (version, siblingVersion, clientConfigs) => {
    const response = assignment(version, Buffer.from(`value-${version}`))
    const metadata = JSON.parse(Buffer.from(response.targets, "base64"))
    const bytes = Buffer.from(`sibling-${siblingVersion}`)
    metadata.signed.targets[sibling] = {
      custom: { v: siblingVersion },
      hashes: { sha256: createHash("sha256").update(bytes).digest("hex") },
      length: bytes.length
    }
    response.targets = Buffer.from(JSON.stringify(metadata)).toString("base64")
    response.target_files.push({ path: sibling, raw: bytes.toString("base64") })
    response.client_configs = clientConfigs
    const delivery = reactor.poll(client, response)
    assert.equal(delivery.batchFullyWatched, true)
    reactor.call("rc_view_release", delivery.batch)
  }

  /** @param {string} reason @param {string[]} expected */
  const observe = (reason, expected) => {
    const update = reactor.call("rc_watch_next", client, watch)
    const descriptor = reactor.json("rc_view_describe", update)
    assert.equal(descriptor.reason, reason)
    assert.deepEqual(descriptor.records.map(record => record.file.path), expected)
    const snapshot = reactor.call("rc_view_snapshot", update)
    assert.deepEqual(reactor.json("rc_view_describe", snapshot).records.map(record => record.file.path), [path, sibling])
    reactor.call("rc_view_release", snapshot)
    return update
  }

  /** @param {number} update @param {number[]} plan */
  const finish = (update, plan) => {
    const buffer = reactor.input(Buffer.from(plan))
    try {
      assert.equal(reactor.call("rc_watch_finish", client, watch, update, buffer), 2)
    } finally {
      reactor.call("rc_free", buffer)
    }
  }
  const decisions = [0, 0, 0, 0, 0, 1, 0, 0, 0, 0]
  const accepted = [0, ...decisions]
  const failed = [2, 4, 0, 0, 0, ...Buffer.from("busy"), ...decisions]
  submit(1n, 1, [path, sibling])
  finish(observe("initial", [path, sibling]), accepted)
  submit(2n, 2, [sibling, path])
  finish(observe("changed", [sibling, path]), failed)
  assert.equal(reactor.poll(client, {}).batch, 0)
  finish(observe("retry", [sibling, path]), accepted)
  submit(2n, 3, [path, sibling])
  submit(3n, 3, [path, sibling])
  finish(observe("changed", [sibling, path]), accepted)
  reactor.call("rc_close", client)
})

testBackends("signals ordered overflow and drains the old ticket after retarget", reactor => {
  const client = reactor.open()
  const watch = reactor.withJson({ delivery: "orderedState", products: ["APM_TRACING"], maxPending: 1 },
    buffer => reactor.call("rc_watch_open", client, buffer))
  const submit = (version, expected = true) => {
    const { batch, batchFullyWatched } = reactor.poll(client, assignment(BigInt(version), Buffer.from(`value-${version}`)))
    assert.equal(batchFullyWatched, expected || undefined)
    reactor.call("rc_view_release", batch)
  }
  submit(1)
  const first = reactor.call("rc_watch_next", client, watch)
  submit(2)
  submit(3, false)
  const pinned = Array.from({ length: 31 }, () => reactor.call("rc_snapshot", client))
  assert.equal(reactor.call("rc_watch_next", client, watch), -2)
  assert.equal(reactor.call("rc_watch_replay", client, watch), -2)
  for (const snapshot of pinned) reactor.call("rc_view_release", snapshot)
  reactor.withJson({ target: options.target, identity: options.identity },
    buffer => reactor.call("rc_retarget", client, buffer))
  submit(4)
  assert.equal(reactor.call("rc_watch_next", client, watch), 0)
  const buffer = reactor.input(Buffer.from([0]))
  assert.equal(reactor.call("rc_watch_finish", client, watch, first, buffer), 0)
  reactor.call("rc_free", buffer)
  const reassigned = reactor.call("rc_watch_next", client, watch)
  assert.equal(reactor.json("rc_view_describe", reassigned).reason, "initial")
  reactor.call("rc_close", client)
  assert.throws(() => reactor.json("rc_view_describe", reassigned), /invalid or released view/)
})

testBackends("delivers a bounded result for a large HTTP error and can poll again", reactor => {
  const client = reactor.open()
  const response = Buffer.alloc(16 * 1024 * 1024, "\0é")
  response.write("upstream failure: ")

  const failure = reactor.poll(client, response, 502)
  assert.equal(failure.batch, 0)
  assert.match(failure.error, /upstream failure: /)
  assert.ok(Buffer.byteLength(failure.error) <= 4096)
  assert.equal(failure.error.includes("\uFFFD"), false)
  assert.equal(reactor.call("rc_result", client), 0)
  assert.equal(reactor.poll(client, {}, 204).error, undefined)
  reactor.call("rc_close", client)
})

testBackends("delivers a bounded result for a large transport error and can poll again", reactor => {
  const client = reactor.open()
  const { httpId, timerId } = reactor.start(client)
  const message = Buffer.alloc(16 * 1024 * 1024, "\0é")
  message.write("transport failed: ")
  const handle = reactor.input(message)
  reactor.call("rc_http_fail", client, httpId, handle)
  reactor.call("rc_free", handle)
  assert.deepEqual(reactor.json("rc_event", client), { kind: "cancel", id: timerId })

  const failure = reactor.json("rc_result", client)
  assert.equal(failure.batch, 0)
  assert.match(failure.error, /transport failed: /)
  assert.ok(Buffer.byteLength(failure.error) <= 4096)
  assert.equal(failure.error.includes("\uFFFD"), false)
  assert.equal(reactor.call("rc_result", client), 0)
  assert.equal(reactor.poll(client, {}, 204).error, undefined)
  reactor.call("rc_close", client)
})

testBackends("reports the parse cause for large malformed metadata without echoing the input", reactor => {
  const client = reactor.open()
  const malformed = Buffer.alloc(3 * 1024 * 1024).toString("base64")
  for (const kind of ["targets", "roots"]) {
    const response = assignment(0n)
    response[kind] = kind === "targets" ? malformed : [malformed]
    const rejected = reactor.poll(client, response)
    assert.equal(rejected.batch, 0)
    assert.equal(rejected.error.includes("\0"), false)
    assert.match(rejected.error, /expected value at line 1 column 1/)
    const report = reactor.poll(client, {}, 204)
    assert.equal(report.error, undefined)
    assert.equal(report.request.client.state.has_error, true)
    assert.match(report.request.client.state.error, /expected value at line 1 column 1/)
    assert.equal(reactor.poll(client, {}).request.client.state.has_error, false)
  }
  reactor.call("rc_close", client)
})

testBackends("reports a large receipt diagnostic without blocking the next request", reactor => {
  const client = reactor.open()
  const { batch } = reactor.poll(client, assignment(1n, Buffer.from("assigned")))
  const receipt = reactor.call("rc_view_file", batch, 0, 0)
  const diagnostic = Buffer.alloc(3 * 1024 * 1024)
  diagnostic.write("installer failure: é🙂")
  const message = reactor.input(diagnostic)
  assert.equal(reactor.call("rc_apply", client, receipt, 3, message), 1)
  reactor.call("rc_free", message)
  reactor.call("rc_free", receipt)
  reactor.call("rc_view_release", batch)

  const report = reactor.poll(client, {}, 204)
  const [state] = report.request.client.state.config_states
  assert.equal(state.apply_state, 3)
  assert.equal(state.apply_error ?? "", "")
  reactor.call("rc_close", client)
})

testBackends("preserves complete structured receipt diagnostics that fit in the request", reactor => {
  const client = reactor.open()
  const { batch } = reactor.poll(client, assignment(1n, Buffer.from("assigned")))
  const receipt = reactor.call("rc_view_file", batch, 0, 0)
  const report = { rules: { errors: { "invalid rule": ["rule-id".repeat(1024)] } } }
  const diagnostic = reactor.input(Buffer.from(JSON.stringify(report)))
  assert.equal(reactor.call("rc_apply", client, receipt, 3, diagnostic), 1)
  reactor.call("rc_free", receipt)
  reactor.call("rc_free", diagnostic)
  reactor.call("rc_view_release", batch)

  const next = reactor.poll(client, {}, 204)
  const [state] = next.request.client.state.config_states
  assert.equal(state.apply_state, 3)
  assert.deepEqual(JSON.parse(state.apply_error), report)
  reactor.call("rc_close", client)
})

testBackends("keeps receipt states when escaped diagnostics exceed the request limit", reactor => {
  const client = reactor.open()
  const { batch } = reactor.poll(client, assignment(1n, Buffer.from("assigned")))
  const receipt = reactor.call("rc_view_file", batch, 0, 0)
  const identity = { ...options.identity, extraServices: [""] }
  const headroom = 16 * 1024
  identity.extraServices[0] = "s".repeat(16 * 1024 * 1024 - JSON.stringify(identity).length - headroom)
  reactor.withJson(identity, buffer => reactor.call("rc_set_identity", client, buffer))
  const before = reactor.poll(client, {}, 204)
  assert.equal(before.request.client.state.config_states[0].apply_state, 1)

  const diagnostic = reactor.input(Buffer.alloc(4096))
  assert.equal(reactor.call("rc_apply", client, receipt, 3, diagnostic), 1)
  reactor.call("rc_free", diagnostic)
  reactor.call("rc_free", receipt)
  reactor.call("rc_view_release", batch)

  const report = reactor.poll(client, {}, 204)
  const [state] = report.request.client.state.config_states
  assert.equal(state.apply_state, 3)
  assert.equal(state.apply_error ?? "", "")
  reactor.withJson(options.identity, buffer => reactor.call("rc_set_identity", client, buffer))
  const recovered = reactor.poll(client, {}, 204).request.client.state.config_states[0]
  assert.equal(recovered.apply_state, 3)
  assert.equal(recovered.apply_error, "\0".repeat(4096))
  reactor.call("rc_close", client)
})

testBackends("reports oversized request metadata without enqueueing an HTTP event", reactor => {
  const client = reactor.open()
  const identity = { ...options.identity, extraServices: [""] }
  identity.extraServices[0] = "s".repeat(16 * 1024 * 1024 - JSON.stringify(identity).length)
  reactor.withJson(identity, buffer => reactor.call("rc_set_identity", client, buffer))

  reactor.call("rc_poll", client)
  assert.equal(reactor.call("rc_event", client), 0)
  assert.match(reactor.json("rc_result", client).error, /request body exceeds/)
  reactor.withJson(options.identity, buffer => reactor.call("rc_set_identity", client, buffer))
  assert.equal(reactor.poll(client, {}, 204).error, undefined)
  reactor.call("rc_close", client)
})

testBackends("preserves multiple complete receipt diagnostics when they fit", reactor => {
  const client = reactor.open()
  const response = assignment(1n, Buffer.from("assigned"))
  const metadata = JSON.parse(Buffer.from(response.targets, "base64").toString())
  const [original] = response.target_files
  const count = 17
  for (let index = 1; index < count; index++) {
    const sibling = `datadog/2/APM_TRACING/sibling-${index}/config`
    metadata.signed.targets[sibling] = metadata.signed.targets[path]
    response.target_files.push({ path: sibling, raw: original.raw })
    response.client_configs.push(sibling)
  }
  response.targets = Buffer.from(JSON.stringify(metadata)).toString("base64")

  const { batch } = reactor.poll(client, response)
  const text = "é".repeat(2048)
  const diagnostic = reactor.input(Buffer.from(text))
  for (let index = 0; index < count; index++) {
    const receipt = reactor.call("rc_view_file", batch, index, 0)
    assert.equal(reactor.call("rc_apply", client, receipt, 3, diagnostic), 1)
    reactor.call("rc_free", receipt)
  }
  reactor.call("rc_free", diagnostic)
  reactor.call("rc_view_release", batch)

  const report = reactor.poll(client, {}, 204)
  const states = report.request.client.state.config_states
  assert.equal(states.length, count)
  for (const { apply_state, apply_error } of states) {
    assert.equal(apply_state, 3)
    assert.equal(apply_error, text)
  }
  reactor.call("rc_close", client)
})

testBackends("keeps the last verified assignment and retries integrity reports", assertAgentIntegrity)

function assertAgentNoChange(reactor) {
  const client = reactor.open()
  for (const status of [204, 404]) {
    const cold = reactor.poll(client, Buffer.alloc(0), status)
    assert.equal(cold.error, undefined)
    assert.equal(cold.ready, false)
    assert.equal(cold.batch, 0)
  }
  const initial = reactor.poll(client, assignment(1n, Buffer.from("verified")))
  const receipt = reactor.call("rc_view_file", initial.batch, 0, 0)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 1)
  reactor.call("rc_view_release", initial.batch)

  for (const [status, body] of [[204, Buffer.alloc(0)], [404, { targets: "ignored" }]]) {
    const retained = reactor.poll(client, body, status)
    assert.equal(retained.error, undefined)
    assert.equal(retained.ready, true)
    assert.equal(retained.batch, 0)
    assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 1)
    const next = reactor.poll(client, {})
    assert.equal(next.request.client.state.has_error, false)
    assert.deepEqual(next.request.client.state.config_states, retained.request.client.state.config_states)
    assert.equal(next.request.client.state.config_states[0].apply_state, 2)
    const snapshot = reactor.call("rc_snapshot", client)
    assert.deepEqual(reactor.file(snapshot), Buffer.from("verified"))
    reactor.call("rc_view_release", snapshot)
  }
  reactor.call("rc_free", receipt)
  reactor.call("rc_close", client)
}

testBackends("retains the Agent assignment and receipts on HTTP 204 and 404", assertAgentNoChange)

testBackends("Agent reissues the same cached payload at a new version", reactor => {
  const client = reactor.open()
  const payload = Buffer.from("cached payload")
  const first = reactor.poll(client, assignment(1n, payload))
  const receipt = reactor.call("rc_view_file", first.batch, 0, 0)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 1)

  const cached = assignment(2n, payload)
  cached.target_files = []
  const unresolved = reactor.poll(client, {
    ...cached,
    client_configs: [...cached.client_configs, "datadog/2/APM_TRACING/absent/config"]
  })
  assert.match(unresolved.error, /isn.t stored/)
  assert.equal(unresolved.batch, 0)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 1)

  const replacement = reactor.poll(client, cached)
  assert.equal(replacement.error, undefined)
  assert.notEqual(replacement.batch, 0)
  assert.equal(reactor.json("rc_view_describe", first.batch).records[0].file.version, "1")
  const record = reactor.json("rc_view_describe", replacement.batch).records[0]
  assert.equal(record.file.version, "2")
  assert.equal(record.previous.version, "1")
  assert.deepEqual(reactor.file(first.batch), payload)
  assert.deepEqual(reactor.file(replacement.batch), payload)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 0)
  const pending = reactor.poll(client, {})
  assert.equal(pending.request.client.state.config_states[0].version, 2)
  assert.equal(pending.request.client.state.config_states[0].apply_state, 1)
  const newReceipt = reactor.call("rc_view_file", replacement.batch, 0, 0)
  assert.equal(reactor.call("rc_apply", client, newReceipt, 2, 0), 1)
  const unchanged = reactor.poll(client, cached)
  assert.equal(unchanged.batch, 0)
  assert.equal(unchanged.request.client.state.config_states[0].apply_state, 2)
  reactor.call("rc_free", newReceipt)
  reactor.call("rc_free", receipt)
  reactor.call("rc_view_release", first.batch)
  reactor.call("rc_view_release", replacement.batch)
  reactor.call("rc_close", client)
})

testBackends("preserves the current cache and receipt when the Agent assignment cannot be resolved", reactor => {
  const client = reactor.open()
  const payload = Buffer.from("accepted payload")
  const first = reactor.poll(client, assignment(1n, payload))
  const receipt = reactor.call("rc_view_file", first.batch, 0, 0)
  const accepted = reactor.poll(client, {}).request.client.state
  const absent = "datadog/2/APM_TRACING/absent/config"
  const replacement = assignment(2n, Buffer.from("new payload"), "next-cursor")
  replacement.client_configs.push(absent)
  const removal = assignment(2n, undefined, "next-cursor")
  removal.client_configs.push(absent)

  for (const rejected of [replacement, removal]) {
    const result = reactor.poll(client, rejected)
    assert.match(result.error, /isn.t stored/)
    assert.equal(result.batch, 0)
    assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 1)
    const state = reactor.poll(client, {}).request.client.state
    assert.equal(state.has_error, true)
    assert.deepEqual(state.backend_client_state, accepted.backend_client_state)
    assert.deepEqual(state.config_states.map(config => [config.version, config.apply_state]), [[1, 2]])
  }

  const recovered = reactor.poll(client, assignment(2n, Buffer.from("new payload")))
  const record = reactor.json("rc_view_describe", recovered.batch).records[0]
  assert.equal(record.file.version, "2")
  assert.equal(record.previous.version, "1")
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 0)
  reactor.call("rc_free", receipt)
  reactor.call("rc_view_release", first.batch)
  reactor.call("rc_view_release", recovered.batch)
  reactor.call("rc_close", client)
})

testBackends("does not dispatch host operations cancelled before the host reads them", reactor => {
  const client = reactor.open()
  reactor.call("rc_poll", client)
  reactor.call("rc_cancel", client)
  assert.equal(reactor.call("rc_event", client), 0)
  assert.match(reactor.json("rc_result", client).error, /cancelled/)

  reactor.call("rc_poll", client)
  const dispatched = reactor.json("rc_event", client)
  assert.equal(dispatched.kind, "http")
  reactor.call("rc_free", dispatched.body)
  reactor.call("rc_cancel", client)
  assert.deepEqual(reactor.json("rc_event", client), { kind: "cancel", id: dispatched.id })
  assert.equal(reactor.call("rc_event", client), 0)
  assert.match(reactor.json("rc_result", client).error, /cancelled/)
  reactor.call("rc_close", client)
})

testBackends("cancels host operations and rejects late completions without consuming their input", reactor => {
  const client = reactor.open()
  const target = {
    target: { ...options.target, service: "new-service" },
    identity: options.identity
  }
  const retarget = () => reactor.withJson(target, input => reactor.call("rc_retarget", client, input))
  assert.equal(retarget(), 1)
  const timeout = reactor.start(client)
  assert.equal(timeout.request.client.client_tracer.service, "new-service")
  assert.throws(retarget, /poll is in progress/)
  assert.throws(() => reactor.call("rc_timer_elapsed", client, timeout.httpId), /not a pending timer/)
  reactor.call("rc_timer_elapsed", client, timeout.timerId)
  assert.deepEqual(reactor.json("rc_event", client), { kind: "cancel", id: timeout.httpId })
  assert.throws(retarget, /previous poll result has not been read/)
  assert.match(reactor.json("rc_result", client).error, /timed out/)

  const lateBody = reactor.input(Buffer.from("{}"))
  assert.throws(() => reactor.withJson({ status: 200 }, metadata =>
    reactor.call("rc_http_succeed", client, timeout.httpId, metadata, lateBody)), /not a pending HTTP/)
  assert.equal(reactor.call("rc_buffer_len", lateBody), 2)
  reactor.call("rc_free", lateBody)

  const pending = reactor.start(client)
  reactor.call("rc_cancel", client)
  const cancelled = [reactor.json("rc_event", client), reactor.json("rc_event", client)]
  assert.deepEqual(new Set(cancelled.map(event => event.id)), new Set([pending.httpId, pending.timerId]))
  assert.ok(cancelled.every(event => event.kind === "cancel"))
  assert.match(reactor.json("rc_result", client).error, /cancelled/)
  assert.equal(reactor.poll(client, {}).ready, false)
  reactor.call("rc_close", client)
})

testBackends("reports an HTTP transport failure and retains the verified revision", reactor => {
  const client = reactor.open()
  const previous = Buffer.from("verified-before-transport-failure")
  const initial = reactor.poll(client, assignment(1n, previous))
  reactor.call("rc_view_release", initial.batch)

  const { httpId, timerId } = reactor.start(client)
  const message = Buffer.from("fixture transport failure")
  const messageId = reactor.input(message)
  reactor.call("rc_http_fail", client, httpId, messageId)
  assert.equal(reactor.call("rc_buffer_len", messageId), message.length)
  reactor.call("rc_free", messageId)
  assert.deepEqual(reactor.json("rc_event", client), { kind: "cancel", id: timerId })
  const failed = reactor.json("rc_result", client)
  assert.equal(failed.batch, 0)
  assert.equal(failed.ready, true)
  assert.match(failed.error, /fixture transport failure/)
  const snapshot = reactor.call("rc_snapshot", client)
  assert.deepEqual(reactor.file(snapshot), previous)
  reactor.call("rc_view_release", snapshot)

  const recovered = reactor.poll(client, {})
  assert.equal(recovered.error, undefined)
  assert.equal(recovered.ready, true)
  assert.equal(recovered.batch, 0)
  reactor.call("rc_close", client)
})

/** @param {Reactor} reactor */
function assertOptionalAgentCursor (reactor) {
  const client = reactor.open()
  const initial = reactor.poll(client, assignment(1n, Buffer.from("original")))
  reactor.call("rc_view_release", initial.batch)

  const cursor = [...Buffer.from("test-state")]
  const variants = [
    { custom: { opaque_backend_state: 42 }, version: 2n, rejected: true },
    { custom: undefined, version: 2n, rejected: false },
    { custom: { agent_refresh_interval: 30 }, version: 3n, rejected: false }
  ]
  for (const { custom, version, rejected } of variants) {
    const contents = Buffer.from(`revision ${version}`)
    const response = assignment(version, contents)
    const metadata = JSON.parse(Buffer.from(response.targets, "base64").toString())
    metadata.signed.custom = custom
    response.targets = Buffer.from(JSON.stringify(metadata)).toString("base64")
    const result = reactor.poll(client, response)
    if (rejected) {
      assert.match(result.error, /invalid type/)
      assert.equal(result.batch, 0)
    } else {
      assert.equal(result.error, undefined)
      assert.deepEqual(reactor.file(result.batch), contents)
      reactor.call("rc_view_release", result.batch)
    }
    const next = reactor.poll(client, {}).request.client.state
    assert.deepEqual(next.backend_client_state, rejected ? cursor : [])
    assert.equal(next.config_states[0].version, rejected ? 1 : Number(version))
  }
  reactor.call("rc_close", client)
}

/** @param {Reactor} reactor */
function assertSignedAgentless (reactor) {
  const fixture = agentlessFixture(path)
  const client = reactor.open({ ...options, ...fixture.options })
  const requests = []
  let configRequest = Buffer.alloc(0)

  /** @param {Uint8Array} protobuf */
  function exchange (protobuf) {
    requests.length = 0
    const deadlines = new Set()
    reactor.call("rc_poll", client)
    for (let index = 0; index < 24; index++) {
      const event = reactor.json("rc_event", client)
      if (event === undefined) {
        assert.equal(deadlines.size, 0)
        const result = reactor.json("rc_result", client)
        assert.ok(result)
        return result
      }

      switch (event.kind) {
        case "sleep":
          if (event.delayMs === options.timeoutMs) {
            deadlines.add(event.id)
          } else {
            assert.ok(event.delayMs >= 30_000)
            reactor.call("rc_timer_elapsed", client, event.id)
          }
          break
        case "cancel":
          assert.ok(deadlines.delete(event.id))
          break
        case "http": {
          const url = new URL(event.url)
          assert.equal(url.origin, "https://config.test.invalid")
          const headers = Object.fromEntries(event.headers)
          assert.equal(headers["dd-api-key"], fixture.options.apiKey)
          assert.equal(headers["content-type"], "application/x-protobuf")
          requests.push(url.pathname)
          let body
          if (url.pathname === "/api/v0.1/org") {
            assert.equal(event.method, "GET")
            assert.equal(event.body, 0)
            body = fixture.org
          } else {
            assert.equal(url.pathname, "/api/v0.1/configurations")
            assert.equal(event.method, "POST")
            configRequest = reactor.take(event.body)
            assert.ok(configRequest.length > 0)
            body = protobuf
          }
          const response = reactor.input(body)
          reactor.withJson({ status: 200 }, metadata => reactor.call("rc_http_succeed", client, event.id, metadata, response))
          break
        }
        default:
          assert.fail(`unexpected event: ${event.kind}`)
      }
    }
    assert.fail("Agentless poll did not settle")
  }

  const identity = { ...options.identity, extraServices: [""] }
  const limit = 16 * 1024 * 1024
  const identitySize = JSON.stringify(identity).length
  identity.extraServices[0] = "s".repeat(limit - identitySize)
  reactor.withJson(identity, buffer => reactor.call("rc_set_identity", client, buffer))
  reactor.call("rc_poll", client)
  assert.equal(reactor.call("rc_event", client), 0)
  const invalidStartup = reactor.json("rc_result", client)
  assert.match(invalidStartup.error, /request body exceeds/)
  assert.ok(invalidStartup.nextPollAfterMs >= 30_000 && invalidStartup.nextPollAfterMs <= 60_000)
  reactor.withJson(options.identity, buffer => reactor.call("rc_set_identity", client, buffer))

  const startupFailure = exchange(fixture.response(1, "signature").protobuf)
  assert.deepEqual(requests.toSorted(), ["/api/v0.1/configurations", "/api/v0.1/org"])
  assert.equal(startupFailure.batch, 0)
  assert.equal(startupFailure.ready, false)
  assert.match(startupFailure.error, /signature|threshold/i)
  assert.equal(reactor.call("rc_snapshot", client), 0)

  let revision = fixture.response(1)
  let delivered = exchange(revision.protobuf)
  assert.equal(delivered.ready, true)
  assert.equal(delivered.error, undefined)
  assert.deepEqual(reactor.file(delivered.batch), revision.payload)

  const faults = [
    ["signature", /signature|threshold/i],
    ["hash", /hash/i],
    ["expiry", /expired/i],
    ["organization", /org UUID/i]
  ]
  for (const [index, [fault, reason]] of faults.entries()) {
    const version = index + 2
    const receipt = reactor.call("rc_view_file", delivered.batch, 0, 0)
    reactor.call("rc_view_release", delivered.batch)

    const failed = exchange(fixture.response(version, fault).protobuf)
    assert.equal(failed.batch, 0)
    assert.equal(failed.ready, true)
    assert.match(failed.error, reason)
    const retained = reactor.call("rc_snapshot", client)
    assert.deepEqual(reactor.file(retained), revision.payload)
    reactor.call("rc_view_release", retained)

    revision = fixture.response(version)
    delivered = exchange(revision.protobuf)
    assert.equal(delivered.error, undefined)
    assert.deepEqual(reactor.file(delivered.batch), revision.payload)
    assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 0)
    reactor.call("rc_free", receipt)
  }

  const receipt = reactor.call("rc_view_file", delivered.batch, 0, 0)
  const generation = reactor.json("rc_view_describe", delivered.batch).generation
  const targetGeneration = reactor.withJson({
    target: { ...options.target, service: "new-scope" },
    identity: options.identity
  }, input => reactor.call("rc_retarget", client, input))
  assert.equal(targetGeneration, 2)
  assert.equal(reactor.call("rc_snapshot", client), 0)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 0)
  assert.deepEqual(reactor.file(delivered.batch), revision.payload)

  const pending = exchange(fixture.response(faults.length + 1, "omitted-director-targets").protobuf)
  assert.equal(pending.ready, false)
  assert.equal(pending.batch, 0)
  assert.equal(pending.error, undefined)
  assert.equal(reactor.call("rc_snapshot", client), 0)

  const assigned = exchange(revision.protobuf)
  assert.equal(assigned.ready, true)
  assert.equal(assigned.error, undefined)
  const next = reactor.json("rc_view_describe", assigned.batch)
  assert.ok(BigInt(next.generation) > BigInt(generation))
  assert.equal(next.targetGeneration, "2")
  assert.equal(next.records[0].previous, undefined)
  assert.deepEqual(reactor.file(assigned.batch), revision.payload)
  assert.equal(reactor.call("rc_apply", client, receipt, 2, 0), 0)
  const currentReceipt = reactor.call("rc_view_file", assigned.batch, 0, 0)
  assert.equal(reactor.call("rc_apply", client, currentReceipt, 2, 0), 1)
  const fileVersion = faults.length + 2
  const cached = fixture.response(fileVersion, undefined, fileVersion - 1)
  const reissued = exchange(cached.protobuf)
  assert.equal(reissued.error, undefined)
  assert.notEqual(reissued.batch, 0)
  const updated = reactor.json("rc_view_describe", reissued.batch).records[0]
  assert.equal(updated.file.version, String(fileVersion))
  assert.equal(updated.previous.version, String(fileVersion - 1))
  assert.deepEqual(reactor.file(reissued.batch), revision.payload)
  assert.equal(reactor.call("rc_apply", client, currentReceipt, 2, 0), 0)
  const reissuedReceipt = reactor.call("rc_view_file", reissued.batch, 0, 0)
  assert.equal(reactor.call("rc_apply", client, reissuedReceipt, 2, 0), 1)
  const diagnosticStart = "invalid AppSec rules: "
  const diagnostic = Buffer.from(diagnosticStart + "rule-id ".repeat(4096))
  const diagnosticHandle = reactor.input(diagnostic)
  assert.equal(reactor.call("rc_apply", client, reissuedReceipt, 3, diagnosticHandle), 1)
  reactor.call("rc_free", diagnosticHandle)

  identity.extraServices[0] = "s".repeat(limit - 16 * 1024 - identitySize)
  reactor.withJson(identity, buffer => reactor.call("rc_set_identity", client, buffer))
  assert.equal(exchange(cached.protobuf).error, undefined)
  assert.ok(configRequest.length <= limit)
  assert.equal(configRequest.includes(diagnosticStart), false)
  const rejectedState = Buffer.from("\x1a\x0bAPM_TRACING\x20\x03")
  assert.ok(configRequest.includes(rejectedState))

  reactor.withJson(options.identity, buffer => reactor.call("rc_set_identity", client, buffer))
  assert.equal(exchange(cached.protobuf).error, undefined)
  assert.ok(configRequest.includes(diagnostic))
  reactor.call("rc_free", reissuedReceipt)
  reactor.call("rc_free", currentReceipt)
  reactor.call("rc_free", receipt)
  reactor.call("rc_view_release", reissued.batch)
  reactor.call("rc_view_release", assigned.batch)
  reactor.call("rc_view_release", delivered.batch)
  reactor.call("rc_close", client)
}

testBackends("does not start an Agentless request with an invalid trust root", reactor => {
  const fixture = agentlessFixture(path)
  const client = reactor.open({ ...options, ...fixture.options, directorRoot: "{" })
  reactor.call("rc_poll", client)
  assert.equal(reactor.call("rc_event", client), 0)
  const failed = reactor.json("rc_result", client)
  assert.equal(failed.ready, false)
  assert.equal(failed.batch, 0)
  assert.match(failed.error, /EOF/)
  reactor.call("rc_close", client)
})

testBackends("Agentless verifies signed assignments and keeps the last trusted revision through failures",
  assertSignedAgentless)
testBackends("Agent accepts assignments without cursor metadata", assertOptionalAgentCursor)

/** @param {string} name @param {(reactor: Reactor) => void} run */
function testBackends (name, run) {
  test(`${name} (WASI)`, () => run(new Reactor()))
  test(`${name} (native FFI)`, {
    skip: nativeArtifact === undefined && "Set DD_RC_TEST_NATIVE to a native reactor library"
  }, () => withNativeReactor(run))
}

/** @param {(reactor: Reactor) => void} run */
async function withNativeReactor (run) {
  const ffi = await import("node:ffi")
  const scalarNames = Object.keys(schema.exports).filter(name => name !== "rc_buffer_ptr" && name !== "rc_version")
  const signatures = Object.fromEntries(scalarNames.map(name => [`ddog_${name}`, {
    return: "int32", arguments: ["pointer", ...schema.exports[name].map(() => "int32")]
  }]))
  const { lib, functions } = ffi.dlopen(nativeArtifact, {
    ...signatures,
    ddog_rc_version: { return: "int32", arguments: [] },
    ddog_rc_reactor_new: { return: "pointer", arguments: [] },
    ddog_rc_reactor_drop: { return: "void", arguments: ["pointer"] },
    ddog_rc_buffer_bytes: { return: "pointer", arguments: ["pointer", "int32"] },
    ddog_rc_input_bytes: { return: "pointer", arguments: ["pointer", "int32"] }
  })
  try {
    assert.equal(functions.ddog_rc_version(), 1)
    const handle = functions.ddog_rc_reactor_new()
    assert.notEqual(handle, 0n)
    try {
      const exports = Object.fromEntries(scalarNames.map(name => [name, functions[`ddog_${name}`].bind(undefined, handle)]))
      run(new Reactor({
        exports,
        read (buffer, length) {
          const pointer = functions.ddog_rc_buffer_bytes(handle, buffer)
          assert.notEqual(pointer, 0n)
          return ffi.toBuffer(pointer, length, true)
        },
        write (buffer, bytes) {
          const pointer = functions.ddog_rc_input_bytes(handle, buffer)
          assert.notEqual(pointer, 0n)
          ffi.exportArrayBufferView(bytes, pointer, bytes.byteLength)
        }
      }))
    } finally {
      functions.ddog_rc_reactor_drop(handle)
    }
  } finally {
    lib.close()
  }
}
