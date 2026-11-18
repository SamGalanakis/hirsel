# Static HTTP assets

[HTTP](http.md). [Specification](../../../docs/spec/http.md#http-14-static-asset-manifest).

Generate a module from the declared asset directory before building the server.

```sh
python3 core/tools/http_assets.py web/ > app/embedded_assets.hirsel
```

The module contains each original path, an alias fingerprinted from its
identity SHA-256, the media type, bytes, and one strong ETag per representation.
The generator embeds identity and smaller gzip representations. Pass
`--brotli PROGRAM` to embed smaller Brotli representations as well.

Build the generated `manifest()` once and put it in the router's shared
state. Register a GET route and return `Reply(assets.respond(req, manifest)?)`
from its handler. The assets need no file capability. The response shares
its selected representation with the manifest and sends it through the
connection's bounded writer.

The original name uses `no-cache`. Its fingerprinted alias uses
`public, max-age=31536000, immutable`. The selected coding determines the
ETag and responses include `Vary: Accept-Encoding`. A matching
`If-None-Match` gives 304. HEAD follows the router's normal handling and
omits the body. Range leaves the response whole.
