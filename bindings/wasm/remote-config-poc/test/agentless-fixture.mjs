// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import { createHash, generateKeyPairSync, sign } from 'node:crypto'

/** @typedef {string | number | boolean | null | Json[] | {[key: string]: Json}} Json */

/** @param {Json} value */
function canonical (value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`
  if (value !== null && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`
  }
  return JSON.stringify(value)
}

/** @param {number} value */
function varint (value) {
  const result = []
  do {
    const byte = value & 127
    value >>>= 7
    result.push(byte | (value ? 128 : 0))
  } while (value)
  return Buffer.from(result)
}

/** @param {number} field @param {string | Uint8Array} value */
function bytes (field, value) {
  const buffer = Buffer.from(value)
  return Buffer.concat([varint(field * 8 + 2), varint(buffer.length), buffer])
}

/** @param {Uint8Array} value */
function hash (value) { return createHash('sha256').update(value).digest('hex') }

function key () {
  const pair = generateKeyPairSync('ed25519')
  const publicKey = pair.publicKey.export({ format: 'der', type: 'spki' }).subarray(-32).toString('hex')
  const metadata = { keytype: 'ed25519', scheme: 'ed25519', keyid_hash_algorithms: ['sha256', 'sha512'], keyval: { public: publicKey } }
  return { ...pair, metadata, id: hash(Buffer.from(canonical(metadata))) }
}

/** @param {Json} signed @param {ReturnType<typeof key>} signer */
function metadata (signed, signer) {
  const signature = sign(null, Buffer.from(canonical(signed)), signer.privateKey).toString('hex')
  return Buffer.from(canonical({ signed, signatures: [{ keyid: signer.id, sig: signature }] }))
}

/** @param {number} version @param {Uint8Array} raw */
function top (version, raw) { return Buffer.concat([varint(8), varint(version), bytes(2, raw)]) }

/** @param {Uint8Array} raw @param {number} version */
function description (raw, version) { return { version, length: raw.length, hashes: { sha256: hash(raw) } } }

const configPath = 'datadog/2/APM_TRACING/poc/config'
const expiry = '2099-01-01T00:00:00Z'

/** @param {{options: object}} scenario */
export function agentlessFixture (scenario) {
  const config = key()
  const director = key()
  const product = key()
  /** @param {ReturnType<typeof key>} signer */
  function root (signer) {
    return metadata({ _type: 'root', spec_version: '1.0', version: 1, expires: expiry,
      consistent_snapshot: true, keys: { [signer.id]: signer.metadata },
      roles: Object.fromEntries(['root', 'timestamp', 'snapshot', 'targets'].map(role =>
        [role, { keyids: [signer.id], threshold: 1 }])) }, signer)
  }
  const configRoot = root(config)
  const directorRoot = root(director)
  /** @param {number} version @param {string} fault */
  function response (version, fault = '') {
    const content = Buffer.from(JSON.stringify({ sample_rate: version / 10 }))
    const target = { length: content.length, hashes: { sha256: hash(content) }, custom: { v: version } }
    const base = { _type: 'targets', spec_version: '1.0', version, expires: fault === 'expired' ? '2000-01-01T00:00:00Z' : expiry }
    let delegated = metadata({ ...base, targets: { [configPath]: target } }, product)
    if (fault === 'signature') {
      const value = JSON.parse(delegated)
      value.signatures[0].sig = '00'.repeat(64)
      delegated = Buffer.from(canonical(value))
    }
    const configTargets = metadata({ ...base, expires: expiry, targets: {}, delegations: {
      keys: { [product.id]: product.metadata }, roles: [{ name: 'APM_TRACING', keyids: [product.id],
        threshold: 1, terminating: true, paths: ['datadog/*/APM_TRACING/*/*'] }]
    } }, config)
    const directorTargets = metadata({ ...base, expires: expiry, targets: { [configPath]: target } }, director)
    /** @param {ReturnType<typeof key>} signer @param {Buffer} targets @param {boolean} delegatedTargets */
    function repository (signer, targets, delegatedTargets) {
      const meta = { 'targets.json': description(targets, version) }
      if (delegatedTargets) meta['APM_TRACING.json'] = description(delegated, version)
      const snapshot = metadata({ _type: 'snapshot', spec_version: '1.0', version, expires: expiry, meta }, signer)
      const timestamp = metadata({ _type: 'timestamp', spec_version: '1.0', version, expires: expiry,
        meta: { 'snapshot.json': description(snapshot, version) } }, signer)
      return Buffer.concat([bytes(2, top(version, timestamp)), bytes(3, top(version, snapshot)), bytes(4, top(version, targets))])
    }
    const configMetas = Buffer.concat([repository(config, configTargets, true),
      bytes(5, Buffer.concat([varint(8), varint(version), bytes(2, 'APM_TRACING'), bytes(3, delegated)]))])
    const file = Buffer.concat([bytes(1, configPath), bytes(2, fault === 'hash' ? Buffer.from('0'.repeat(content.length)) : content)])
    return Buffer.concat([bytes(1, configMetas), bytes(2, repository(director, directorTargets, false)), bytes(3, file)]).toString('base64')
  }
  return {
    options: { ...scenario.options, url: 'https://poc.invalid', apiKey: 'poc-api-key', hostname: 'poc-host',
      configRoot: configRoot.toString(), directorRoot: directorRoot.toString() },
    actions: [
      { kind: 'cancel' },
      { kind: 'fixture', body: response(1) },
      { kind: 'ack', value: { path: configPath, state: 2 } },
      { kind: 'fixture', body: response(2, 'signature') },
      { kind: 'fixture', body: response(2) },
      { kind: 'fixture', body: response(3, 'hash') },
      { kind: 'fixture', body: response(3) },
      { kind: 'fixture', body: response(4, 'expired') },
      { kind: 'fixture', body: response(4) }
    ]
  }
}
