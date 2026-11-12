# hirsel-core-ids

Neutral identities: `ContentHash`, `PackageId`, `ItemPath`, `DefPath`. Both the old
front end (through `hirsel-syntax`, which re-exports them until M3) and the new core
crates name definitions with these types. Arena-local and session IDs stay with their
owners; file IDs and spans are `hirsel-span`.
