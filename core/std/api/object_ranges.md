# Object ranges

`range_span` resolves an HTTP byte-range request against the object size and returns half-open bounds. `content_range` parses a response of the form `bytes first-last/size` and rejects reversed or out-of-bounds ranges. `decimal` parses an unsigned 64-bit decimal field.

`read_batch` issues buffered reads in groups of up to `width` requests and collects replies in input order. It reserves one pending slot per active lane. With zero lanes it returns `Precondition` and the supplied buffer for each request.
