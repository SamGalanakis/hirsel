# Fonts

Self-hosted so the site makes no third-party requests. Started from the Google Fonts latin subsets.

- `archivo-black-latin.woff2`: Archivo Black, SIL Open Font License 1.1.
- `inter-latin.woff2`: Inter (variable, full `wght` axis kept, weights 400 to 600 used), SIL Open Font License 1.1.

Both are subset further to printable ASCII plus U+00A0 and a short list of typographic marks
(quotes, dashes, ellipsis, bullet, primes, guillemets, ×, ·, °, ©, ®, ™, €, £, −). Every layout
feature that applies to the kept glyphs (kern, tnum, frac and the rest), the `wght` axis, hinting
and the name records are kept. A character outside that set (an accented letter,
say) falls back to the next font in the CSS stack, so a copy change that adds one must
re-subset from the Google Fonts latin files:

```
pyftsubset <font>.woff2 --unicodes=U+0020-007E,U+00A0,U+00A3,U+00A9,U+00AB,U+00AD,U+00AE,U+00B0,U+00B7,U+00BB,U+00D7,U+2002,U+2009,U+2013,U+2014,U+2018,U+2019,U+201A,U+201C,U+201D,U+201E,U+2022,U+2026,U+2032,U+2033,U+2039,U+203A,U+20AC,U+2122,U+2191,U+2193,U+2212,<new> \
  --layout-features='*' --name-IDs='*' --name-languages='*' --notdef-outline --flavor=woff2 --output-file=<font>.woff2
```
