# Float

[API overview](../API.md). [Operations and contracts](../float.hirsel).

This module extends core's float conversions with decimal parsing, formatting
and numeric helpers. Parsing at each width rounds directly to that width.
The decimal parser uses an exact fast path, Eisel-Lemire multiplication and a
bounded stack decimal fallback, with no allocation. The source gives the grammar,
range errors and formatting rules for each operation.

float's push_fixed64 and its push_ascii helper build their digits as Bytes
and pass them through `string.from_utf8`, revalidating about 20 ASCII bytes per number that
they wrote themselves. That revalidation stays in std (no core entry point) until it shows in a
profile.

`push_json_f64` uses the same shortest round-trip digits as `push_f64`.
It spells them as serde_json does, with a fractional zero for finite
integers in decimal form, signed exponents in scientific form, and `null`
for nonfinite values. `std.json.write_f64` uses this formatter.

`shortest32` returns the coefficient and exponent of the shortest round-trip
decimal. The binary32 width bounds the cost of its integer products and
digit removal. It allocates nothing.

`sin`, `cos`, and `sin_cos` take radians and use Rust libm's binary64
kernels and argument reduction. `sin_cos` returns the sine first and shares
one reduction. All finite inputs, including signed zero and the largest
finite magnitudes, follow the same implementation on the evaluator,
Cranelift, and LLVM. Infinities and NaN return canonical NaN.
