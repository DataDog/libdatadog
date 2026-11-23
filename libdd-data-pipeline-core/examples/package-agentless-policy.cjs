// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

"use strict"

const { copyFileSync, mkdirSync, readFileSync, writeFileSync } = require("node:fs")
const { join } = require("node:path")

const [wasmPath, output] = process.argv.slice(2)
const bytes = readFileSync(wasmPath)
mkdirSync(output, { recursive: true })
writeFileSync(join(output, "agentless_policy.wasm"), bytes)
copyFileSync(join(__dirname, "agentless-policy.cjs"), join(output, "agentless-policy.cjs"))
writeFileSync(
  join(output, "agentless-policy-bytes.cjs"),
  `// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

'use strict'

module.exports = '${bytes.toString("base64")}'
`,
)
