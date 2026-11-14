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
