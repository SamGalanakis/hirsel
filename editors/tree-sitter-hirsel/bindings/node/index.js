const path = require('node:path');

module.exports = require('node-gyp-build')(path.join(__dirname, '../..'));
module.exports.nodeTypeInfo = require('../../src/node-types.json');
module.exports.data.nodeTypeInfo = require('../../data/src/node-types.json');
