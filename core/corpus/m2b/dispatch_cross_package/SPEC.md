# M2b cross-package generic StaticFn dispatch

`app` calls generic `StaticFn[F]` functions from `lib`, where the
generic function and its caller are in different packages. The evaluator and both
native engines must agree on the identity bytes.

## cross_len = 103

`len_of` returns the 103-byte identity of `repeat`.

## cross_same = 1

`same` compares that identity with the bytes produced in the caller and returns 1.

## cross_sum = 488096

`sum_of` computes the weighted sum of the same identity bytes.

## cross_eq_same = 1

A `StaticFn` of `lib.echo` made in `app` equals the one `lib` makes of the same
function: equal identity bytes (PROG-9), whichever unit made the value.

## cross_eq_other = 0

A `StaticFn` of `lib.mute` made in `app` is not equal to `lib`'s `StaticFn` of `echo`, though both have the same body: a
name is part of the identity bytes and a body is not.

## Hook D typed registration

`cross_owned_number = 41`: package lib's generic codec-bounded adapter calls the
app's owned u64 handler on 40, giving 41. `cross_owned_text = 2`: its other
instance takes and returns the owned String "ok", whose two UTF-8 bytes survive.
Both refund all admissions.

`cross_owned_identity = 835002`: lib's generic typed descriptor delegates to
its generic identity checksum. Apply sum(byte[i] * (i + 1)) independently to
0x01 followed by `app@1/main/owned_number(take u64)->core@1/core/Result[u64,core@1/core/Capacity]`
and to 0x01 followed by `app@1/main/owned_text(take core@1/core/String)->core@1/core/Result[core@1/core/String,core@1/core/Capacity]`, then add the two sums.
These are PROG-9's definition paths and full signatures, written by hand; no engine output is an oracle.
