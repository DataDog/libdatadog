import { createHash, generateKeyPairSync, sign } from "node:crypto"

const expiry = "2099-01-01T00:00:00Z"
const orgUuid = "test-org"
const delegation = "APM_TRACING"

/** @typedef {string | number | boolean | null | Json[] | {[key: string]: Json}} Json */

/** @param {Json} value */
function canonical (value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`
  if (value !== null && typeof value === "object") {
    const entries = Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`)
    return `{${entries.join(",")}}`
  }
  return JSON.stringify(value)
}

/** @param {number} value */
function varint (value) {
  const result = []
  do {
    const byte = value & 127
    value >>>= 7
    result.push(value === 0 ? byte : byte | 128)
  } while (value !== 0)
  return Buffer.from(result)
}

/** @param {number} field @param {string | Uint8Array} value */
function bytes (field, value) {
  const buffer = Buffer.from(value)
  return Buffer.concat([varint(field * 8 + 2), varint(buffer.length), buffer])
}

/** @param {Uint8Array} value */
function hash (value) {
  return createHash("sha256").update(value).digest("hex")
}

function newSigner () {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519")
  const metadata = {
    keytype: "ed25519",
    scheme: "ed25519",
    keyid_hash_algorithms: ["sha256", "sha512"],
    keyval: { public: publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("hex") }
  }
  return { privateKey, metadata, id: hash(Buffer.from(canonical(metadata))) }
}

/** @param {Json} signed @param {ReturnType<typeof newSigner>} signer */
function envelope (signed, signer) {
  const signature = sign(null, Buffer.from(canonical(signed)), signer.privateKey).toString("hex")
  return Buffer.from(canonical({ signed, signatures: [{ keyid: signer.id, sig: signature }] }))
}

/** @param {ReturnType<typeof newSigner>} signer */
function root (signer) {
  return envelope({
    _type: "root",
    spec_version: "1.0",
    expires: expiry,
    version: 1,
    consistent_snapshot: true,
    keys: { [signer.id]: signer.metadata },
    roles: Object.fromEntries(["root", "timestamp", "snapshot", "targets"].map(role =>
      [role, { keyids: [signer.id], threshold: 1 }]))
  }, signer).toString()
}

/** @param {Uint8Array} raw @param {number} version */
function describe (raw, version) {
  return { version, length: raw.length, hashes: { sha256: hash(raw) } }
}

/** @param {number} field @param {number} version @param {Uint8Array} raw */
function top (field, version, raw) {
  return bytes(field, Buffer.concat([varint(8), varint(version), bytes(2, raw)]))
}

/**
 * @param {string} path
 * @param {(version: number) => Json} [valueForVersion]
 * @param {number} [refreshSeconds]
 */
export function agentlessFixture (path, valueForVersion = version => ({ sample_rate: version / 10 }), refreshSeconds) {
  const config = newSigner()
  const director = newSigner()
  const product = newSigner()

  /**
   * @param {number} version
   * @param {'signature' | 'expiry' | 'hash' | 'organization' | 'omitted-director-targets'} [fault]
   * @param {number} [cachedPayloadVersion]
   */
  function response (version, fault, cachedPayloadVersion) {
    const payload = Buffer.from(JSON.stringify(valueForVersion(cachedPayloadVersion ?? version)))
    const target = { ...describe(payload, version), custom: { v: version } }
    const format = { spec_version: "1.0", version, expires: expiry }
    const base = { ...format, _type: "targets" }
    const delegated = envelope({
      ...base,
      expires: fault === "expiry" ? "2000-01-01T00:00:00Z" : expiry,
      targets: { [path]: target }
    }, fault === "signature" ? director : product)
    const configTargets = envelope({
      ...base,
      targets: {},
      delegations: {
        keys: { [product.id]: product.metadata },
        roles: [{
          name: delegation,
          keyids: [product.id],
          threshold: 1,
          terminating: true,
          paths: ["datadog/*/APM_TRACING/*/*"]
        }]
      }
    }, config)
    const directorMetadata = { ...base, targets: { [path]: target } }
    const directorTargets = envelope(refreshSeconds === undefined
      ? directorMetadata
      : { ...directorMetadata, custom: { agent_refresh_interval: refreshSeconds } }, director)

    /**
     * @param {ReturnType<typeof newSigner>} signer
     * @param {Uint8Array} targets
     * @param {boolean} isConfig
     */
    function repository (signer, targets, isConfig) {
      const meta = { "targets.json": describe(targets, version) }
      const products = []
      const custom = isConfig
        ? { org_uuid: fault === "organization" ? "other-org" : orgUuid }
        : {}
      if (isConfig) {
        meta[`${delegation}.json`] = describe(delegated, version)
        products.push(bytes(5, Buffer.concat([varint(8), varint(version), bytes(2, delegation), bytes(3, delegated)])))
      }
      const snapshot = envelope({ ...format, _type: "snapshot", meta, custom }, signer)
      const timestamp = envelope({
        ...format,
        _type: "timestamp",
        meta: { "snapshot.json": describe(snapshot, version) }
      }, signer)
      const metadata = [top(2, version, timestamp), top(3, version, snapshot)]
      if (isConfig || fault !== "omitted-director-targets") {
        metadata.push(top(4, version, targets))
      }
      return Buffer.concat([...metadata, ...products])
    }

    const file = Buffer.concat([bytes(1, path), bytes(2, fault === "hash" ? Buffer.alloc(payload.length) : payload)])
    return {
      payload,
      protobuf: Buffer.concat([
        bytes(1, repository(config, configTargets, true)),
        bytes(2, repository(director, directorTargets, false)),
        cachedPayloadVersion === undefined ? bytes(3, file) : Buffer.alloc(0)
      ])
    }
  }

  return {
    options: {
      url: "https://test.invalid",
      apiKey: "test-api-key",
      hostname: "wasi-host",
      configRoot: root(config),
      directorRoot: root(director)
    },
    org: bytes(1, orgUuid),
    response
  }
}
