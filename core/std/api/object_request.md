# S3 request and response helpers

The object client uses this module to map operations to HTTP methods, encode
paths and headers, interpret responses, and accumulate request counts. Use
[`std.object`](object.md#request-counters) to read counts from a running client.
