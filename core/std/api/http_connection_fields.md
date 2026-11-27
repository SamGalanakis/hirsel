# HTTP connection fields

[HTTP server](http.md). [HTTP head parser](http1.md).

`valid_authority` checks a Host field for an HTTP authority with an optional
numeric port. `relay_fields` copies end-to-end fields from the source bytes
and their complete parsed request head in receipt order. It uses the same
field policy as `http.relay_fields`.
