# Char

[API overview](../API.md). [Operations and contracts](../char.hirsel).

This module extends core's `char` operations. A char is a four-byte plain,
key and ordered value. Helpers classify ASCII, convert ASCII case, read a digit
at a radix, and determine the UTF-8 width without allocation.

`char.UNICODE_VERSION` is `(18, 0, 0)`. `char.category` returns a `char.Category`, and the Unicode classifiers allocate nothing.
