# hirsel-core-layout

Layout recipes for declared shapes (own, resource, stable, scoped and `@compact(W)`
stable types) and the logical storage policy `charge-v1` (MEM-24). Types and recipe
computation only. See `src/recipe.rs` for the native and compact layout rules and
the `hirsel-core-ir` crate docs for the calling convention. Status: FORM v1 draft; not frozen until the settled S2 rules.

`extend(shapes, recipes)` computes only shapes appended after an unchanged recipe
prefix. Inline dependencies may name that prefix or other appended shapes. The
suffix publishes only after every new recipe succeeds, so a refused layout leaves
the prefix usable. `compute` is the same operation with an empty prefix. The front
end uses this when its body-local type table grows; it retains each old recipe.
