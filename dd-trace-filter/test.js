const { Filter } = require('./pkg')

const filter = new Filter('world')

const buffer = new Uint8Array([ 12, 34, 56, 78, 90 ])
console.log(filter.filter(buffer))
