# F19 — accepted duplicate form field names overwrite user values

Recommend; high confidence, medium priority. Owner C24, worker C24-01 ../workers/C24-VIEWS-INSTRUMENTS.md. Independently reopened validate_form, individual field validation, inline/template view acceptance, FormNode seeding/update/submission and CATALOG's keyed-data contract.

A valid-looking tool spec with two fields named answer passes validation. FormNode assigns seed[name] twice, renders both inputs from the same map slot and submits one answer key; independent user values cannot survive. Inline model-authored specs are a supported input path, and no current consumer supports repeated-name multi-value semantics. Thread-instrument forms already reject duplicate names, corroborating the intended invariant.

Target add per-form uniqueness validation after existing field validation, rejecting the second name with its path before ViewUpsert; retain ordered field array and name-keyed values. No new form representation/table/wire fields or client normalization. Scope templates/spec.rs, focused inline/file-backed validation and distinct-field submission tests, plus CATALOG uniqueness wording. Preserve same names across separate forms. Audit ran no tests. Coordinate with active F13/F14 view lane without duplicating its placement/order work.

Coordinator rechecked exact current anchors: templates/spec.rs::validate_form lines222–238 and app/src/views/ViewRenderer.tsx::FormNode lines580–615. The seed assignment, update and submission all share the same name-keyed slot; there is no duplicate-name reducer.
