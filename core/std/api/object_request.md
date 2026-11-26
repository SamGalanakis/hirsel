# S3 request accounting

The object client uses this module to map operations to HTTP methods and
accumulate request counts. Use [`std.object`](object.md#request-counters)
to read counts from a running client.
