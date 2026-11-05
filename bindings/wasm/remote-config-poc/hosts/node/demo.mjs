// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import { readFileSync } from 'node:fs'

import { input, load, poll, resolveOutcome } from './client.mjs'

const [wasm, url, scenarioPath] = process.argv.slice(2)
const scenario = JSON.parse(readFileSync(scenarioPath, 'utf8'))
const abi = load(wasm)
const client = input(abi, { ...scenario.options, url, language: 'nodejs' }, abi.exports.rc_create)
try {
  for (const action of scenario.actions) {
    let outcome
    if (action.kind === 'ack') {
      input(abi, action.value, buffer => abi.exports.rc_ack(client, buffer))
      outcome = { acknowledged: true }
    } else if (action.kind === 'cancel' || action.kind === 'timeout') {
      abi.check(abi.exports.rc_poll(client))
      const events = abi.json(abi.exports.rc_events(client))
      const operation = events.find(event => event.kind === (action.kind === 'timeout' ? 'sleep' : 'http'))
      if (!operation) throw new Error('Expected pending operation')
      if (action.kind === 'cancel') {
        abi.check(abi.exports.rc_cancel(client))
      } else {
        input(abi, {}, buffer => abi.exports.rc_complete(client, operation.id, buffer))
      }
      outcome = resolveOutcome(abi, abi.json(abi.exports.rc_result(client)))
      const reply = abi.write({ status: 200, body: abi.writeBytes(Buffer.from('e30=', 'base64')) })
      try {
        if (abi.exports.rc_complete(client, operation.id, reply) !== -1) throw new Error('Late completion accepted')
      } finally { abi.check(abi.exports.rc_free(reply)) }
      const cancellations = abi.json(abi.exports.rc_events(client))
      if (!cancellations.some(event => event.kind === 'cancel')) throw new Error('No host cancellation')
    } else if (action.kind === 'fixture') {
      abi.check(abi.exports.rc_poll(client))
      const requests = []
      for (let step = 0; step < 32; step++) {
        const events = abi.json(abi.exports.rc_events(client))
        for (const event of events) {
          if (event.kind === 'sleep' && !events.some(value => value.kind === 'http')) {
            input(abi, {}, buffer => abi.exports.rc_complete(client, event.id, buffer))
          }
          if (event.kind !== 'http') continue
          requests.push({ ...event, body: Buffer.from(abi.read(event.body)).toString('base64') })
          const body = new URL(event.url).pathname === '/api/v0.1/org' ? '' : action.body
          input(abi, { status: 200, body: abi.writeBytes(Buffer.from(body, 'base64')) }, buffer => abi.exports.rc_complete(client, event.id, buffer))
        }
        const result = abi.check(abi.exports.rc_result(client))
        if (result) {
          outcome = { outcome: resolveOutcome(abi, abi.json(result)), requests }
          break
        }
      }
      if (!outcome) throw new Error('Fixture did not complete')
    } else if (action.kind === 'poll') {
      outcome = await poll(abi, client)
    } else {
      throw new Error(`Unknown scenario action: ${action.kind}`)
    }
    process.stdout.write(`${JSON.stringify(outcome)}\n`)
  }
} finally {
  abi.check(abi.exports.rc_close(client))
}
