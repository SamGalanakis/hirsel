# YAML Test Suite vectors

The bundled inputs and expected events come from
[yaml/yaml-test-suite, data-2022-01-17](https://github.com/yaml/yaml-test-suite/tree/data-2022-01-17).
The file keeps the input bytes, the error flag, and a projection of test.event.
The projection decodes scalar escapes, applies YAML 1.2 Core Schema resolution
and expands aliases. It preserves document order, collection structure, and
arbitrary mapping keys. Each accepted tree also goes through the emitter and
is parsed again for structural equality.

The release contains 402 cases. The law covers 289 valid trees and all 95
upstream error cases. It also checks typed refusal for recursive aliases,
unknown tags, duplicate keys, bytes, steps, and depth. The source license is
reproduced in LICENSE-vectors.

## Exclusions

These 18 valid event streams have a duplicate Core Schema mapping key or
use an explicit tag outside the supported Core tags.

- 2JQS. duplicate Core Schema mapping key.
- 2XXW. non-Core tag tag:yaml.org,2002:set.
- 565N. non-Core tag tag:yaml.org,2002:binary.
- 5TYM. non-Core tag !my-light.
- 6CK3. non-Core tag !local.
- 6WLZ. non-Core tag !foo.
- 7FWL. non-Core tag !bar.
- 9WXW. non-Core tag !foo.
- C4HZ. non-Core tag tag:clarkevans.com,2002:shape.
- CC74. non-Core tag tag:example.com,2000:app/foo.
- CUP7. non-Core tag !local.
- J7PZ. non-Core tag tag:yaml.org,2002:omap.
- M5C3. non-Core tag !foo.
- P76L. non-Core tag tag:example.com,2000:app/int.
- UGM3. non-Core tag tag:clarkevans.com,2002:invoice.
- X38W. duplicate Core Schema mapping key.
- Z67P. non-Core tag !foo.
- Z9M4. non-Core tag tag:example.com,2000:app/foo.
