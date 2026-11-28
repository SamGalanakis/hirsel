# data

`data.witness("orders_v1")` binds a literal module alias to the concrete record in its expected type, such as `data.Witness[Site]`. The compiler follows the record's declarations and supplies its descriptor and construction layout.

`data.decode(witness, input, limits)` reads one qualified record through the end of the borrowed bytes. It returns an owned `Result[T, data.DataError]`. Lists and maps retain document order. Missing optional fields take `None` and omitted scalar fields take their declared defaults.

`data.Limits` bounds input bytes, nesting depth, syntax nodes, collection length, decoded payload bytes, key comparison bytes and diagnostics. An error gives an invalid-data or limit reason, the schema field path and a half-open byte span. A duplicate map key also identifies the earlier key's span. Paths contain only schema field names.

Task byte and step bounds apply to decoding. A failed admission raises `Capacity`. A step refusal reaches the current step bound. Both release the decoder's scratch space and any partially constructed result.
