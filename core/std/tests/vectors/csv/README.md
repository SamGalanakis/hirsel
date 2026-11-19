# CSV sources

- `csvs/` and `json/` contain the paired input and expected records from
  [csv-spectrum 2.0.0](https://github.com/maxogden/csv-spectrum/tree/d30e80f8b99d2eecb3778f1d7b9ed1cb425502ec),
  commit `d30e80f8b99d2eecb3778f1d7b9ed1cb425502ec`. Max Ogden's
  [package.json](https://github.com/maxogden/csv-spectrum/blob/d30e80f8b99d2eecb3778f1d7b9ed1cb425502ec/package.json#L14)
  declares BSD-2-Clause. The first record is the header from the JSON keys.
- The `rust_csv_reader` table adapts the
  [rust-csv reader tests](https://github.com/BurntSushi/rust-csv/blob/05612e87e6e92910d40d7214b70952b8434fef9b/src/reader.rs)
  and [csv-core reader tests](https://github.com/BurntSushi/rust-csv/blob/05612e87e6e92910d40d7214b70952b8434fef9b/csv-core/src/reader.rs),
  commit `05612e87e6e92910d40d7214b70952b8434fef9b`, MIT.

The Rust reader strips an initial BOM, skips empty records and permits
quotes in unquoted fields. Hirsel keeps the BOM as part of the first
field, returns zero-field records for empty lines and rejects an
unquoted quote. Both accept the unequal-width example when the Rust
reader uses `flexible(true)`. The Hirsel record API leaves width
validation to the caller. See [the CSV grammar](../../../api/csv.md#grammar).

`location_coordinates.csv` contains a bare quote in an unquoted field.
Hirsel checks the documented refusal at that quote. The upstream JSON
fixture also differs from the CSV input's phone number.
