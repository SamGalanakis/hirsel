# HTTP request encoding

[HTTP client](http_client.md) defines the supported operations.
This module implements the request-head encoder behind them: it writes a head into
caller-owned fixed storage and marks where each secret goes for `tls.send_secret`.
