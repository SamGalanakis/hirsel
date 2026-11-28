# Package registry

Phase 1 specifies online publication and named selection. The registry service, admission
policy, log writer and bucket protocol are Hirsel programs. The Rust toolchain checks library
content in a PACK-3 worker. A client needs only HTTPS GET on the static plane to verify a
release. The public origin is `https://pkg.hirsel.ai`. An installation can serve its own
origin with its own trust configuration. These are build contracts for Phase 1.

## 1. Names, bytes and ownership

Use the scope, identity, SemVer, binding tags, limits and package root P of
[Packages §1](packages.md#1-canonical-library-content). For `github.com/acme/json@1` the
scope is `github.com/acme`. For `acme.dev/json@1` it is `acme.dev`. Only scoped names can
be published. Every calculation of P, including in an audience, hashes the *expanded* C using
`Root::of`: SHA-256 over the length-framed domain `hirsel.library-root.v1` and length-framed
C. The frame lengths are little-endian u64s. Token, intent and sidecar digests use ordinary
SHA-256 of the specified bytes. A digest is 64 lowercase hex characters here, including P in
object keys and protocol fields. Manifests keep the `sha256:` prefix.

In the schemas below, brackets denote fixed-position JSON arrays, except that a list marked
`*` has zero or more elements. All protocol JSON bytes are RFC 8785 canonical UTF-8, with no
final newline. Reject unknown tags, extra/missing fields, duplicates, invalid UTF-8 and
noncanonical encodings. Binary in JSON is padded, canonical RFC 4648 base64. Counts, indices,
generations and Unix seconds in log and intent schemas are unsigned decimal **strings**, with no redundant zero and at
most the u64 maximum. Sets are distinct and sorted by canonical element bytes. `null` is
literal JSON null. A registry origin is an HTTPS origin with lowercase DNS host, no userinfo,
path, query or fragment and the default port omitted. Protocol paths below start at that origin.

A scope has an immutable spelling and an initial nonempty set of subjects. Each subject can
authorize an operation. Its wire value is one of:

| Subject | Fields and equality |
|---|---|
| workload | `["oidc",issuer,id,workflow_ref]`, pinned HTTPS issuer, tagged positive decimal `owner/ID` or `repo/ID`, and exact workflow selector |
| signing key | `["ed25519",key]`, the 32 public key bytes |

The issuer profile specifies which signed claims supply the **numeric** identifier and workflow
selector. For GitHub Actions, use `repository_owner_id` or `repository_id` respectively.
Match the token's `workflow_ref` against its signed `repository` prefix and use
`<repository_id>/<workflow path>@<full git ref>` as the selector. Thus an owner login change
preserves the selector, and another repository of the same owner cannot supply it. A reusable
workflow claim alone cannot identify the calling repository. Unknown issuers or claim profiles
refuse. For example, a workflow at
`acme/build/.github/workflows/publish.yml@refs/heads/main` in repository 123 keeps selector
`123/.github/workflows/publish.yml@refs/heads/main` after an org rename. A different forge
can be enrolled by an authorized rebind.

**Enrollment.** Read the folded release log and the first-bind reservation at `s/<scope>`.
For an unclaimed forge tenant, require the signed current owner login to match TENANT and the
workflow to be explicitly authorized by the organization for *scope enrollment*. The forge
profile must verify that delegation. The GitHub organization profile reads the public
organization record directly from the pinned GitHub API, checks its login and numeric id
against the signed owner claims, and requires its owner-controlled description to contain
the complete standalone line `hirsel-enroll-v1=<digest>`. The digest commits to the canonical
enrollment array below. This lets the organization delegate just this subject set without
handing the registry an administrator credential. An ordinary repository job cannot supply
the organization's endorsement. Other forge profiles require equivalent administrator proof.
For a custom domain read `https://HOST/.well-known/hirsel-registry.json` over certificate-verified
HTTPS, no redirects, from a public address for the public service. Its value is
`["hirsel-enroll-v1",origin,scope,subjects]`. Compare all fields, require proof of
possession by one listed subject and bind exactly that set. A forge authorization names the
same fields and subjects. The first successful reservation fixes that enrollment. On `Exists`,
read it and use only its subjects until the first bind commits. Afterward use only the logged
generation. DNS control, a recycled login, or an unsigned replacement document cannot reset
it. No lost-key reset exists. A currently authorized subject signs a rebind of the same scope
from its current generation to the complete next set. The old set authorizes the change;
generation increases by one and releases keep their original spelling.

**Intent.** The bytes to authorize are
`["hirsel-intent-v1",origin,action,scope,generation,root,body]`. For a release, action is
`publish`, root is P, and body is `[identity,version,description,source_commit,enrollment]`;
enrollment is the initial subject set for first use, otherwise null. For `scope-rebind`,
root is null and body is the new subject set. For each status action, root is the affected P
and body is `[previous_event,payload]` as in §3. The generation is zero for enrollment, one
for the initial committed bind. Metadata is covered even when a retry uses another token.
An Ed25519 publisher signs these exact bytes. A workload requests a token with a single
audience equal to `origin + "/intent/" + action + "/" + (root or "-") + "/" + hex(SHA256(intent))`.
Use the actual compact JWS ASCII bytes for `token_sha256`.

Verify with the configured issuer's public JWKS over TLS and its pinned asymmetric algorithm
allowlist, using `std.jwt.jwks_public` and `verify_jwt_public`. Require an unambiguous key,
issuer, exact audience, unexpired exp, valid nbf when present, iat no later than admission,
and the signed subject claims. No key URL or algorithm supplied by the token selects the
trust policy. For keys, use `ed25519.verify`. First-use key publication still requires
enrollment. Persist the signed intent digest and public evidence. For OIDC the log commits
the token digest. Where the issuer's historical keys can be archived, accept the matching
compact token for a public evidence archive **only after expiry**, validated against its
historical keys and admission time. The publisher or a monitor can deposit it after expiry.
Distinguish missing archival evidence from independently reverified evidence. Never persist
a live bearer in a row, pending intent, diagnostic or log. Scope records hold public
identifiers and keys. There are zero publisher accounts, password hashes, reusable tokens or
credential tables. Service-owned bucket, log-signing and witness transport configuration is
outside publisher state.

## 2. Compiler admission

The host explicitly grants a `Compiler` value. Its waiting operation
`check_library(compiler, request) -> Result<CheckedLibrary, Refusal>` runs one isolated,
target-independent check. Calling it reaches the `Compiler` authority kind. It conveys no
general process launch, path lookup, network access or native module admission. Reuse the
[PACK-3 supervisor](packing.md#pack-3-the-compiler-service), seat/queue, bounded diagnostics
and process-group deadline. Its module build request and 16 MiB host input default remain
separate. The following is a typed value schema; `bytes` are length-delimited bytes passed by
value or read-only handle with a checked length, never a caller-supplied host path.

```text
CheckLibrary = { root: digest, content: bytes,
                 closure: [{root: digest, content: bytes}],
                 history: [{origin: origin, identity: id, version: semver, root: digest}] }
CheckedLibrary = { root: digest, header: {identity: id, version: semver, charge: "charge-v1"},
                   bindings: [Binding], interface: InterfaceV1, authority: Card }
Binding = [alias, id, "toolchain"] | [alias, id, "exact", tested_root]
        | [alias, id, "floor", tested_root, bound_version]
Refusal = Rejected(text) | Unsupported(text) | Limit(Input|Cpu|Wall|Memory|Output)
        | Busy | Unavailable(text)
```

The registry constructs `history` **only** from release leaves whose inclusion it verified at
a trusted checkpoint of the registry routed for the child's scope. The uploader may supply
proofs and bytes but cannot assert a verified history row. Supply a row for every distinct floor
target in the closure; supplied rows for exact children must match their decoded headers too. The worker computes every root and decodes every C with the driver's
canonical v3 decoder. Walk the tested graph without substitutions, require complete and
unique matching children, reject extras and cycles, and perform the library front-end check
with the installed toolchain. For **each** floor compare the child's decoded identity,
version and root against both the binding and its verified history tuple. A self-declared
SemVer, a label, or a valid proof from another scope's registry cannot establish a floor.
Exact pins require the matching child's C; toolchain bindings come from the installed
toolchain. No native linking, foreign initialization, package tests or publisher code run.

Return bindings in C order. `InterfaceV1` is the unfiltered interface projection of
`hirsel inspect --interface --json` for the head package (version, package, modules with
path, extends_core and items with definition, name, kind, signature and optional doc).
`Card` has the same fields and meanings as the §11 audit projection: `carried` maps export
to `{takes,returns}`, `reached` maps export to `{kinds,provided,contained,unresolved}`, and
`verdict` is pure, conditional, unknown or the reached-kind list. Compute this from the
checked program. Later instantiations and consumer ceilings still get checked on every build.
Publish the RFC 8785 encoding of the success object as a sidecar. The log binds its digest
and the compiler build identity. The service never implements a second Hirsel decoder for C.

| Resource | Registry check limit |
|---|---|
| Upload | 65 MiB per compressed C frame, 512 MiB total compressed payload; check lengths before allocation |
| Expanded input | 64 MiB inclusive per raw C **at both** ingress and worker; 512 MiB total C, head included, at most 4,096 packages |
| Zstd | One frame per C, declared size, 8 MiB window, no dictionary, checksum when present; enforce size while decoding with the bounded `std.zstd` decoder |
| Worker | 10 CPU seconds, 20,000 ms wall, 512 MiB RAM, 64 MiB aggregate reply, diagnostics at most 64 KiB; bounded service queue as in PACK-3 |

The supervisor checks input totals including a separate 64 MiB ceiling on history metadata.
The OS confines the worker before processing input. Expose only the toolchain, request bytes
and a private scratch directory. Clear inherited environment and file descriptors except the
bounded request/reply channels. Deny socket creation and access to the host network,
credential stores, other jobs, host home, agent sockets and service secrets. On Linux a
read-only mount allowlist, OS network/socket denial and a memory cgroup can enforce this.
Every supported host must enforce equivalent denial and aggregate resource caps or return
`Unavailable` before spawning. An empty environment or a CLI offline flag alone is
insufficient. A cap or isolation failure yields a typed refusal and no registry writes.

The same grant offers `compiler.read_source(c, content, path, limit) -> Result[String,
SourceReadError] waits`. It decodes one expanded C v3 archive with the same decoder,
validates a canonical relative `.hirsel` path, and returns the member's UTF-8 text.
`InvalidPath` covers paths outside the canonical syntax, `MissingPath` covers absent
members, and `MalformedArchive` covers undecodable archives and non-UTF-8 members.
`Limit(Output)` refuses a member larger than the requested byte limit before returning
any text. The worker and queue use the same confinement and resource refusals as
admission. The Registry verifies the release status and the bounded Zstd frame against
its signed release root before requesting the source. The MCP, HTTP and CLI discovery
entries require a `Compiler` grant and cap a source reply at 1 MiB.

## 3. The two logs

The release and status prefixes are `/log` and `/status`, respectively. Their distinct
checkpoint origins are the registry authority (host and nondefault port) followed by the
prefix. Each has a pinned log public key. Use C2SP signed notes, checkpoints, timestamped
Ed25519 witness cosignatures and tlog-tiles. No checkpoint extensions. In each log, entry
bytes E are the canonical JSON row below. The bundle frames E with its two-byte big-endian
length (maximum 65,535). The leaf is SHA-256(0x00 || E), a branch is SHA-256(0x01 || left ||
right), and the empty tree is SHA-256(empty). Use RFC 6962 tree shape and proof order.

| Entry | E, in field order |
|---|---|
| Initial scope | `["scope-bind",scope,subjects,enrollment_digest,time,auth]` |
| Scope change | `["scope-rebind",scope,old_generation,subjects,time,auth]` |
| Release | `["release",identity,version,root,description,source_commit,time,generation,compiler_id,facts_digest,auth]` |
| Retirement | `["retire",root,previous_event,reason,message,time,auth]` |
| Reinstatement | `["reinstate",root,previous_event,message,time,auth]` |
| Advisory | `["advise",root,previous_event,osv_id,osv_digest,time,auth]` |
| Legal removal | `["takedown",root,previous_event,reason,message,time,auth]` |

The first three types belong only to the release log, the last four only to status. `time`
is registry admission time in UTC Unix seconds. `source_commit` is a full lowercase hex Git
object name (40 or 64 digits) or null for a non-Git source. Description, message and reason
are UTF-8 strings. Retirement reason is one of security, deprecated, invalid, renamed, other.
`compiler_id`, `facts_digest`, `enrollment_digest` and `osv_digest` are digests. Enrollment
evidence bytes are the canonical array `[profile,source,document]`: profile is "domain" or
the configured forge issuer, source is the authenticated enrollment URL, and document is the
verified enrollment value from §1.
`auth` is `[subject,intent_digest,token_sha256,signature]` with exactly the field of its
proof kind populated, the other null. Store canonical intent bytes and referenced public
evidence at `e/<digest>`, addressed by ordinary SHA-256. For a status action its intent
payload is the row's fields between previous_event and time, in order, as an array. Its
generation selects the subject set in the release log. Takedown uses a separately configured
registry legal operator key (the route pins its public key as `legal-key`), generation zero. All other status events require scope authority for
the committed release. There is no status for a nonexistent root.

Fold status rows in index order. `previous_event` is null for the first event on that root,
otherwise the leaf hash of the immediately preceding event on it. A retire changes active
to retired, reinstate changes retired to active, and takedown is terminal. An advise upserts
the current OSV record by (root, osv_id) independently of retirement. The public canonical
OSV JSON sidecar must match the digest and id, name the package under ecosystem `Hirsel`,
and cover that exact version in affected versions or a SEMVER range. Preserve OSV aliases,
severity, references, modified and withdrawn fields; a later withdrawal clears that advisory
only. A structurally invalid transition or missing required evidence refuses verified use.
Neither reinstatement nor withdrawal erases history.

At each prefix serve `checkpoint`, `tile/<level>/<index>[.p/<width>]` and
`tile/entries/<index>[.p/<width>]`. Use the C2SP base-1000 three-digit grouping with an
`x` on each nonfinal group, 256 hashes per full tile, partial widths 1–255 and levels spaced
eight binary heights apart. A full entry bundle has 256 framed rows. Serve bytes with C2SP
content types and no redirects. Keep both logs available from index zero. A full tile may
supply the prefix of a historical partial. Immutable tiles and bundles can be cached
indefinitely; checkpoints revalidate within seconds. [Tiles](https://c2sp.org/tlog-tiles) ·
[Checkpoints](https://c2sp.org/tlog-checkpoint) · [Witness protocol](https://c2sp.org/tlog-witness)

**Acceptance.** Verify the log signature, origin and two pinned independent external witness
operators over the *same* checkpoint text in **each** log. Both signed timestamps must be in
the interval [now − 86,400 s, now]. Pin the pair for a trust domain: arbitrary two-of-many
disjoint quorums do not establish a common history. A quiet log periodically resubmits its
unchanged size and root for fresh signatures and CAS-replaces only the signed envelope.
A deployment with no independent witnesses must explicitly advertise and configure
`preview (quorum 0)`; user-visible selection and audit output carries that label. It still
checks log signatures, consistency, inclusion and status replay. Never auto-downgrade on an
outage or count two keys of one operator as independent. Public witnessed selection starts
only with two independent operators. A witnessed checkpoint bounds freshness to 24 hours;
it cannot prove that a registry has received or appended an unlogged complaint.

For each origin keep the last verified checkpoint. A smaller head, a different root at the
same size, or failure to prove RFC 6962 consistency to a larger head refuses. Keep both signed
notes as evidence without replacing the trusted head. Requests for named add, update and
audit verify both logs. Fetch the release row and verify its O(log N) inclusion path against
the release checkpoint, comparing identity, version, P and facts to what was requested.
A label or a version-list hint may locate a leaf but cannot authenticate it. Inclusion does
not prove completeness of version discovery. For status, start at zero on first use, or from
a previously verified local replay state and its exact checkpoint. Fetch **every** successive
entry through the new status size, rederive the RFC 6962 root using the saved frontier, and
apply every transition before using any result. Missing suffixes, gaps, corruption, stale
signatures, wrong origins or an incomplete replay refuse the command. Persist the folded
state, frontier and signed head atomically. A mirror returning an empty per-root status list
cannot conceal a retirement present in the accepted checkpoint. All refusals precede any
manifest or lock write.

## 4. Bucket commit and recovery

Use a bucket with strongly consistent per-key reads, conditional creation and strong ETags.
Writers read the bucket directly. One witnessed checkpoint CAS publishes a nonempty batch in
one log. Search, listings, label reservations, and the existence of a blob or a cosignature
are insufficient to acknowledge it.

| Bucket key | Contents and writer |
|---|---|
| `s/<scope>` | Immutable reservation containing the exact initial scope-bind entry bytes. A rebind appends to the release log; lookup folds from this initial set. |
| `l/<identity>/<version>` | Immutable reservation, ASCII hex P without newline. A committed release leaf must match it. |
| `c/<P>.zst` | One package frame, indexed by its uncompressed library root. Fresh retrieval of taken-down content is disabled. |
| `e/<digest>` | Immutable public evidence or compiler facts. Expired tokens here are raw compact JWS, other evidence has the canonical encoding in §§1–3. |
| `log/pending/<N>`, `status/pending/<N>` | Private, immutable winning proposal at parent size N (ungrouped decimal). |
| `log/tile/…`, `status/tile/…` | Immutable C2SP hash tiles and entry bundles. |
| `log/checkpoint`, `status/checkpoint` | Current signed checkpoint, including required cosignatures. Initialize a signed empty head by conditional creation. |
| `log/heads/<size>`, `status/heads/<size>` | First quorum envelope for a batch ending at size, immutable. It becomes a publication-time receipt only after inclusion in the committed history. |

At the origin, expose the two C2SP prefixes, content and public evidence. Keep reservations
and pending private. The derived static version hint at `/versions/<name>` is a canonical list
of `[version,release_index,first_head_size]` rows. It may be rebuilt by scanning the release
log; lag may delay discovery. A publisher receipt supplies the same locator immediately.
Clients check hints against the logs. The history is sufficient to regenerate every hint.

0. **Admit before any write.** Verify the precise intent and current scope authority,
   capture the parent and authenticated child history, check the Zstd and C limits and P,
   and run §2. Check that header identity and version equal the requested label. Check all row
   sizes, authorized subject, generation, label conflicts and state transition before
   accepting any durable reservation. A refused admission writes nothing. Freeze the entry
   bytes, timestamp and public evidence. For a status change first read the status parent, then authenticate against the
   committed release and scope generation read during this admission. Retain that status
   parent through step 3 or restart admission. A rebind overlapping
   an already admitted status operation can follow that authorization read; a later request
   must use the new generation. A retry at a different status parent needs fresh admission.
1. **Reserve.** For first use, store the public evidence referenced by the initial bind, then
   conditionally create the scope reservation *before* the label. This lets another writer
   finish the reserved bind even if its creator never uploads a frame.
   A losing enrollment re-reads the reservation and refuses a different set. For publication
   `store_no_clobber(l/<identity>/<version>,P)`; on `Exists` fetch and compare. Different P
   is a permanent label conflict. Same P permits authenticated retry; if already committed,
   return its original inclusion receipt. An existing reservation alone never reports
   success. To return
   an original receipt on an idempotent retry, look up and verify inclusion before any blob
   PUT. A status change or rebind needs no new label. Never mutate the initial reservation
   to implement a rebind.
2. **Store input.** With `store_no_clobber` install the verified frame(s) and public evidence
   required for this batch. On `Exists` verify the expanded root and frame limits (two valid
   compressions may differ); for a digest-addressed sidecar verify its bytes. Before step 3,
   a dead writer may leave only private reservations, unreferenced content or hints with no
   valid proof. The same-root publisher can resume with its inputs. No log slot is held and
   unrelated releases can proceed. No helper is required to invent missing upload bytes.
3. **Choose the next leaves.** Read the signed parent and strong ETag together from the
   destination log. Against that exact committed state recheck uniqueness, scope generation,
   status predecessor and admission validity. A first publish batches the saved scope-bind
   then release, in that order, in **one** CAS. If another authorized first publisher has the
   same reservation, it can commit that initial bind with its own release. Later operations
   cite the resulting generation. The proposal at `pending/<N>` has bytes
   `["hirsel-pending-v1",checkpoint_origin,N,parent_tree_hash,[base64(E),…]]`,
   where the hash is hex and the last list is nonempty. Every required input is durable
   before `store_no_clobber` chooses this slot. On `Exists` read the winner. If its
   (origin,N,hash) still equals the current parent, **finish those exact bytes** at steps
   4–6, even on behalf of another request. No re-encoding, clock reading, renewed bearer
   or lease is needed to help. If the parent has advanced, check whether one's operation
   is already included. Otherwise re-admit it at the new parent. A stale writer cannot reuse
   its earlier authorization decision at the next slot.
4. **Materialize.** From the committed prefix and the saved batch derive the unique new
   entry bundle(s), partial/full hash tiles and checkpoint text. Write each new immutable
   object with `store_no_clobber`; on `Exists` verify byte equality. A mismatch is a hard
   integrity failure. No mutable tile can point a client at another successor.
5. **Witness.** Sign the candidate checkpoint text with the log's deterministic Ed25519
   signature. Submit to each configured witness with its last known size and consistency
   proof. Verify every returned cosignature. On a protocol `409 text/x.tlog.size`, parse the
   returned size. If behind the candidate, resubmit from that size with a consistency proof.
   If at the candidate size, resubmit the **same signed head** with old size equal to candidate
   and an empty proof. Only a verified success signature counts. If ahead, re-read the
   committed head and reconcile ancestry; an unreconciled fork or a same-size different hash
   refuses. Create the historical envelope by no-clobber. On `Exists`, verify the same
   signed text and quorum, allowing different valid cosignature timestamps. Refresh expired
   signatures for the current envelope while keeping the first receipt for age.
6. **Publish.** `store_if(checkpoint, Match(parent_etag), witnessed_envelope)` is the commit.
   `Applied` permits acknowledgement with origin, leaf index, P (when applicable) and
   checkpoint. On `Mismatch`, fetch the current head. A heartbeat can change ETag at the
   same size/hash; repeat the CAS with its fresh ETag and sufficiently fresh signatures.
   A later head permits acknowledgement **only** with inclusion of this operation and proof
   of extension. If a helper committed another request, resume the caller's request at step
   0. Status idempotence uses the authorized intent digest and predecessor; once committed,
   replay returns the existing index even if intervening events changed state. A lost reply
   never implies a second publish or status transition.

Apply the same readback rule at every uncertain PUT, including genesis and checkpoint CAS.
The object client can reconcile its own put-id metadata for no-clobber requests; an
`Uncertain` escaping it has no success verdict. Read from the origin. Compare actual bytes,
expanded root, or witnessed ancestry as appropriate. If absent, retry the same conditional
creation; if still at the parent, retry the same conditional CAS. Readback unavailability
leaves the caller unacknowledged. Never substitute an unconditional PUT or a bucket listing.
A helper handles each log independently, including after a crash between witness responses.
A heartbeat races on the current checkpoint's ETag by the same rules, so cannot roll it back.

**G14 fault matrix.** Use persisted bucket bytes and each witness's independent accepted
(size, root) as the oracle; the caller retains a list of received acknowledgements.

| Interrupt or race | Required result after recovery |
|---|---|
| Death after admission, either reservation or blob PUT (0–2) | No premature acknowledgement. Other labels can commit; authenticated retry completes the reserved root. |
| Death after pending, any tile/bundle or either witness reply (3–5) | Another writer finishes the winner byte-for-byte with no original request, including equal-size 409 resubmission. |
| Death immediately before/after checkpoint CAS or after commit before reply (6) | Zero or one committed operation, exactly one after retry, one consistent history containing every acknowledged operation. |
| Lose response on each conditional PUT with the write applied and with it absent | Readback distinguishes the outcomes or leaves the caller unacknowledged. |
| Two writers at one parent, one resuming a full publish late; concurrent heartbeat | No overwrite, duplicate leaf or stale acknowledgement. Both nonconflicting requests can finish. |
| 100 contenders with distinct roots for one label; concurrent enrollment/rebind | Exactly one winning root and immutable label, one valid scope history. A removed subject cannot win at a later release parent. |
| Terminal takedown versus delayed frame PUT; interruption before cleanup | The committed denial and deletion marker survive retries. A fresh fetch gets the logged reason. |

A takedown first commits its terminal status, then withdraws the public object, purges CDN
copies and removes stored package bytes. The origin serves a logged removal reason (HTTP
410). To prevent a delayed step-2 PUT resurrecting a deleted key, replace its frame by a
permanent zero-byte deletion marker using its strong ETag (or create the marker if absent).
The marker is private, never served as a package and never erased by an uploader. Delete
older object versions if the bucket uses versioning. Reconciliation resumes this work from
committed status after a crash. Static serving must enforce the denial even during cleanup.
Fresh verified fetches consult status on a miss; an already cached locked build uses its
verified C offline. Retirement alone does not remove the frame.

## 5. Client selection and transport

Trust configuration belongs to the invoking user's `hirsel/config.hcfg` under XDG_CONFIG_HOME
(default `~/.config`). Each `[[registry.route]]` has `prefix`, `url`, `release-key`,
`status-key`, `legal-key`, `witness-keys` (two pinned C2SP note-verifier keys) and `quorum` (2). An
explicit preview route uses quorum 0 and an empty witness list. Prefix is `*`, `HOST/*` or
`HOST/TENANT/*`; match complete components of the canonical name and use the longest match.
Reject duplicate prefixes, malformed trust or contradictory trust for the same origin.
The built-in default for `*` is the public origin with shipped pins once activated.
A manifest, remote index, redirect or fetched dependency cannot supply trust or reroute
a scope. Never fall back from a failing private route to the public one. Key caches and
verified checkpoints by origin, log and trust policy. Changing a URL for a pinned manifest
source requires an explicit source change; preview state cannot seed witnessed acceptance.

`hirsel registry publish [--workspace]`<!-- doc-ref: planned --> captures and checks the member.
For a registry-derived tested dependency, encode a floor only after checking a stable child
release's inclusion and matching its C. Prerelease and unlogged Git/URL/archive dependencies
use exact pins. The publisher builds those C bytes *before* computing P, compressing and
obtaining root-bound authorization. Deliver the head and the exact-pin unlogged closure
frames needed at the receiving registry; logged children are fetched or verified through
their own trusted routes. A single publish cannot upload under another scope's release label.
The write endpoint is
`POST /v1/submit` over TLS. The body is a four-byte big-endian metadata length, canonical
JSON metadata `[intent,subject,proof,frame_roots,child_releases]`, then for each listed root a four-byte
big-endian frame length and one frame. Metadata is at most 1 MiB. The proof is the compact
JWS or base64 Ed25519 signature. Check the head first, then distinct tested child roots in
byte order. Each child release hint is `[identity,version,root,release_index]` in root order.
The registry routes by identity and verifies inclusion itself; the authenticated rows and
any missing logged frames become §2 inputs. Reject missing, duplicate or unrelated entries.
The worker checks that all floors have history. Non-publish actions have two empty lists. For advise, the canonical OSV
sidecar follows the metadata as a u32-length-prefixed value (at most 1 MiB); other actions
accept no trailer. Success returns `[checkpoint_origin,leaf_index,first_head_size,root]`
using a committed head, with null root for a rebind. An authenticated repeat returns the
same locator. Use 403 for authority, 409 for label or predecessor conflicts, 422 for invalid
input, 413 for size caps and 503 for a temporarily unavailable service or uncertain outcome.
With a workspace, preflight the entire requested member graph for cycles, unpublishable names
and routing. Publish dependency-first, breaking ties by canonical identity. Use the committed
child root and version when packing its parent. Obtain separate authorization per root and
origin. Stop at the first refusal and identify committed members, failed member and unattempted
members. Rerunning confirms existing matching releases before continuing. There is no
workspace-wide commit across labels or registries. `hirsel build --emit recipe --code-store DIR` still emits an executable
release recipe.

Named add accepts `HOST[/TENANT]/NAME`, optionally followed by `@MAJOR` or `@VERSION`.
An unqualified new name takes the highest eligible stable version. Update of an existing
identity stays within its major and the §5 floor/0.minor rules. Explicit local directory paths
use `./` or `../` when they resemble a scoped name. For every involved registry obtain
accepted release/status heads, verify release inclusion, replay all status events, filter
retired/taken-down roots and apply floors, exact pins, compatibility and the §11 ceilings.
Report advisories with OSV IDs. Audit checks the effective graph and reports current statuses
and advisories for affected roots, exiting nonzero on a retired, taken-down or affected
unwithdrawn advisory pin (exit 2). A frozen audit can use an already complete, fresh verified replay
or must refuse before opening a socket. A locked build never performs named selection.

Default minimum age is 72 hours, inclusive at 259,200 seconds. For age evidence verify the
release was included in its first committed covering batch and the historical signed head
extends to the accepted one. The stored first head is log-signed and bears its original
quorum; validate its signatures at publication, without applying today's 24-hour freshness
test to that historical envelope. Use the later of leaf time and the two original witness
times. A monitor compares leaf time with first witnessed inclusion. A later valid covering
head provides a conservative upper bound on publication time. Preview uses the log time and
labels it as unwitnessed. Only a fully specified SemVer bypasses age; it still verifies
inclusion, status, graph, compiler check and authority. Prereleases require an exact version.
A retired pin with no eligible replacement makes plain update fail before any write. If the
sole otherwise eligible fix is young, print its exact upgrade command, e.g.:

```text
json 1.4.0 is retired (security: incorrect input validation).
json 1.4.1 was published 2 h ago, inside the 72 h wait.
To take it now: hirsel update json@1.4.1
```

Build/fetch of registry roots uses one `GET /c/<64hex>.zst` per missing root. Use the
Phase 0 pooled HTTP/1.1 agent (up to eight workers), incremental bounded Zstd, framed
streaming root hash and sharded `xx/<62hex>` store. Verify root, identity and canonical C
before installing. Walk actual selected children. An unlogged exact child's bytes can be
served by its registry carrier without conferring release history on it. Frozen uses the
existing request-scoped transport permission, including resident requests. Reuse immutable
tiles, authenticated replay prefixes, unchanged frames and undamaged local entries. The G3
target is a cold 30-package fetch in ≤1.0 s and ≤2.2 MB transferred. G4 is a one-package
update in ≤300 ms and ≤100 KB. Both use 50 ms RTT and 50 Mbit/s, with verification enabled.
