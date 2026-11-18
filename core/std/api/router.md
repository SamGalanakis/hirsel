# Router

[HTTP](http.md). [Specification](../../../docs/spec/http.md#http-11-router-and-raw-handlers).

`builder` starts a route table. `add(mut builder, method, pattern, take route)`
adds a route with an HTTP method and path pattern. `raw(handler)` supplies
a route with the server's body limit. Set a route's `body_max` to lower it.
The limit applies to the total decoded bytes of a chunked body.
Set `upgrade_bytes` to declare how many bytes an upgraded protocol on
the route may reserve. `raw(handler)` leaves upgrade admission disabled.

Patterns match decoded path segments. A literal can contain percent escapes.
`{name}` captures a single nonempty segment. Final `{name...}` captures
the remaining segments after the preceding slash. Use `http.capture` and
`http.capture_tail` to read those values during the handler.

`build(take builder)` refuses overlapping patterns for the same method
unless one matches a strict subset of the other's paths. The final table
is immutable. The most specific matching route wins regardless of order.

HEAD selects an explicit HEAD route if one matches, then falls back to GET.
Both responses omit the body. An unrouted path gives 404. A path with routes
for other methods gives 405 with Allow. OPTIONS * gives 204 with the server's
Allow. Allow includes HEAD wherever GET is available.
