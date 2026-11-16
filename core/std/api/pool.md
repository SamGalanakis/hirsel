# Pool

[API overview](../API.md). [Operations and contracts](../pool.hirsel).

A Pool is an opaque owner of generational slots with a plain Id containing
its Unique identity, slot index and generation. Every access checks all three;
a wrong identity returns WrongPool and a removed generation returns Removed.
Vacant slots form an intrusive free list, so removal allocates nothing.
Removal increments the generation; a slot at the maximum generation is retired.
Cleaning the pool cleans its live values. Pool itself cannot be cloned.
