// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import { readFileSync } from 'node:fs'
import http from 'node:http'
import https from 'node:https'
import { WASI } from 'node:wasi'

import { RcAbi } from './bindings.mjs'

const MAX_BODY = 16 * 1024 * 1024

/** @param {string} path */
export function load (path) {
  const module = new WebAssembly.Module(readFileSync(path))
  const wasi = new WASI({ version: 'preview1', args: [], env: {}, preopens: {}, returnOnExit: true })
  const instance = new WebAssembly.Instance(module, { wasi_snapshot_preview1: wasi.wasiImport })
  wasi.initialize(instance)
  return new RcAbi(instance.exports)
}

/** @param {RcAbi} abi @param {unknown} value @param {(buffer: number) => number} operation */
export function input (abi, value, operation) {
  const buffer = abi.write(value)
  try { return abi.check(operation(buffer)) } finally { abi.check(abi.exports.rc_free(buffer)) }
}

/**
 * @typedef {{kind: string, id: number, method?: string, url?: string,
 *   headers?: [string, string][], body?: number, milliseconds?: number}} HostEvent
 * @typedef {{status?: number, headers?: [string, string][], body?: Buffer, error?: string}} Reply
 */

/** @param {RcAbi} abi @param {HostEvent} event @param {(reply: Reply) => void} complete */
function request (abi, event, complete) {
  const transport = event.url.startsWith('https:') ? https : http
  const headers = Object.create(null)
  for (const [name, value] of event.headers) (headers[name] ??= []).push(value)
  const request = transport.request(event.url, { method: event.method, headers })
  let terminal = false
  /** @param {Reply} reply */
  function finish (reply) {
    if (terminal) return
    terminal = true
    complete(reply)
  }
  request.once('error', error => finish({ error: error.message }))
  request.once('response', response => {
    const chunks = []
    let length = 0
    response.on('data', chunk => {
      length += chunk.length
      if (length > MAX_BODY) {
        finish({ error: 'response exceeds 16 MiB' })
        response.destroy()
        request.destroy()
      } else {
        chunks.push(chunk)
      }
    })
    response.once('error', error => finish({ error: error.message }))
    response.once('aborted', () => finish({ error: 'response aborted' }))
    response.once('end', () => finish({
      status: response.statusCode,
      headers: Array.from({ length: response.rawHeaders.length / 2 }, (_, index) =>
        [response.rawHeaders[index * 2], response.rawHeaders[index * 2 + 1]]),
      body: Buffer.concat(chunks)
    }))
  })
  request.end(abi.read(event.body))
  return () => { terminal = true; request.destroy() }
}

/** Resolves raw content buffer ids in an outcome into base64 for display; buffers return to the guest. */
export function resolveOutcome (abi, outcome) {
  for (const change of outcome.changes ?? []) {
    change.contentsBase64 = change.contents == null
      ? null
      : Buffer.from(abi.read(change.contents)).toString('base64')
    delete change.contents
  }
  return outcome
}

/** @param {RcAbi} abi @param {number} client @param {AbortSignal} [signal] */
export async function poll (abi, client, signal) {
  abi.check(abi.exports.rc_poll(client))
  const resources = new Map()
  const ready = []
  let wake
  /** @param {{id?: number, reply?: Reply, cancel?: boolean}} value */
  function enqueue (value) {
    ready.push(value)
    wake?.()
  }
  function abort () { enqueue({ cancel: true }) }
  signal?.addEventListener('abort', abort, { once: true })
  if (signal?.aborted) abort()
  try {
    while (true) {
      for (const event of abi.json(abi.exports.rc_events(client))) {
        if (event.kind === 'cancel') {
          resources.get(event.id)?.()
          resources.delete(event.id)
        } else if (event.kind === 'http') {
          resources.set(event.id, request(abi, event, reply => enqueue({ id: event.id, reply })))
        } else if (event.kind === 'sleep') {
          const timer = setTimeout(() => enqueue({ id: event.id, reply: {} }), event.milliseconds)
          timer.unref()
          resources.set(event.id, () => clearTimeout(timer))
        } else {
          throw new Error(`Unknown host event: ${event.kind}`)
        }
      }
      const result = abi.check(abi.exports.rc_result(client))
      if (result) return resolveOutcome(abi, abi.json(result))
      if (!ready.length) await new Promise(resolve => { wake = resolve })
      wake = undefined
      const completion = ready.shift()
      if (completion.cancel) {
        abi.check(abi.exports.rc_cancel(client))
      } else if (resources.delete(completion.id)) {
        // Bodies attach to guest buffers at the loop head only; guest calls never run on
        // socket or timer threads.
        const reply = completion.reply.body === undefined
          ? completion.reply
          : { ...completion.reply, body: abi.writeBytes(completion.reply.body) }
        input(abi, reply, buffer => abi.exports.rc_complete(client, completion.id, buffer))
      }
    }
  } catch (error) {
    // An ordinary adapter failure must not leave a guest future retaining client state.
    abi.exports.rc_cancel(client)
    const result = abi.exports.rc_result(client)
    if (result > 0) resolveOutcome(abi, abi.json(result))
    throw error
  } finally {
    signal?.removeEventListener('abort', abort)
    for (const cancel of resources.values()) cancel()
  }
}
