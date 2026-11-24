# HTTP writer

Writes an HTTP head and body under one absolute deadline. Fixed-length
responses can reuse the connection's output buffer for measured content.
