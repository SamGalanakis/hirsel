module.exports = require('./rules')('hirsel', {
  source_file: $ => seq(
    repeat(choice(seq($._item, choice(';', $._item_newline)), ';')), optional($._item),
  ),
});
