# `std.page_descriptor`

The fixed fields of a collection-map value. `fields` reads a value inside a
page image and `collection_descriptor` reads a value obtained from the tree.
Both reject unknown collection kinds and residency tags. `set_collection_residency`
updates the residency byte of an existing descriptor.
