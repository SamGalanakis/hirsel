# HTTP content codings

[HTTP client Fetch](http_client.md#fetch) defines the supported operations.
This module implements the gzip, deflate and zstd body decoding that `fetch` applies,
under the fetch's body bound and deadline.
