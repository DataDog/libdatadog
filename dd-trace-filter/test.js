const { readFileSync } = require('fs')
const { Filter } = require('./pkg')

const pkg = require('./pkg')
console.log(pkg)

const filter = new Filter()

const before = Uint8Array.from(readFileSync('./src/out.data'))

const after = filter.filterChunk(before)

console.log({
  before,
  after
})
