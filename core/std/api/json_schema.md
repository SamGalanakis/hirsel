# JSON Schema

[JSON](json.md) prepares a codec plan. `write` renders that plan as a JSON
Schema draft 2020-12 document. Annotations select schema objects by RFC 6901
JSON Pointer and become `description` values. An annotation error leaves the
output unchanged.
