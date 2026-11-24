# Simulated S3 head history

The [S3 simulator](s3.md) calls `record(mut writes, request, key, fields, tag,
data)` when it applies a write. For `head` and keys ending in `/head`, it
appends an owned `HeadWrite` with the connection number, key, conditional
request headers, resulting ETag and exact bytes. The report keeps application
order, including writes whose responses never reached the client.
