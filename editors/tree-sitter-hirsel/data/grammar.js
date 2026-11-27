module.exports = require('../rules')('hirsel_data', {
  data_document: $ => seq(field('module', $.identifier), '.', field('record', $.identifier), $.arguments),
}, true);
