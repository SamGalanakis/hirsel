# Dir

[API overview](../API.md). [Operations and contracts](../dir.hirsel).

Paths resolve below the supplied Dir capability and cannot leave its tree.
`create_dir`, `remove_all` and `create_temp` use host primitives with replay points
at issue and delivery. Their source contracts state which host effects remain
when cancellation arrives at delivery. No existing-directory narrowing operation
is provided by this module.
