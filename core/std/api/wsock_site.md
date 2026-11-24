# WebSocket listener HTTP output

`wsock.server_with_site` uses this module for static file responses.
`serve` opens the selected regular file and uses its size for Content-Length.
It sends GET bodies through one buffer whose capacity the caller supplies and
answers HEAD from the file metadata. A missing file yields 404. A failure
before the response starts yields 500. After the head is committed, a failed
transfer closes the connection. When stderr is supplied, a failure emits one
diagnostic.

`static_head` builds the headers for a fixed-length response. `send_response`
sends the header and body under one absolute deadline and handles partial writes.
