# Task

[API overview](../API.md). [Operations and contracts](../task.hirsel).

`task.cancel_join` requests cancellation and consumes the handle to join the
child. A child that already finished successfully returns `Ok(r)`. A child
that ends with `Cancelled` or `Failed` returns its full cause chain, including
any causes added during cleanup. The join is a cancellation point for the caller.
