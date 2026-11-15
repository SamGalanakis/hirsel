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
