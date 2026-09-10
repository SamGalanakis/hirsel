# C06-F1 independent disposition — skip; insufficient materiality for proposed cutover

The worker accurately found that kind, MIME and filename may disagree. I independently reopened ArtifactDraft/validate, artifact_mutation's default MIME derivation, the artifacts DDL and ArtifactSummary, then reran the exact consumer grep (14 matching lines) and checked document/preview/Markdown/download behavior.

The claimed invariant is not established by PRODUCT/ADR0017: kind selects Hirsel's preview mode, MIME supplies download metadata (and a Markdown hint for generic files), and filename is an explicit user-authored download name. Those fields are not necessarily one fact. A caller choosing HTML preview but text/plain download is not demonstrated to be forbidden; a file named .txt may intentionally contain HTML source. The implementation consistently previews by kind and downloads with supplied metadata. Existing bounded filename validation already prevents path/control characters.

The proposed Rust+SQL+wire+TypeScript union and schema cutover would enforce a new product policy, while the report identifies no lost content, access leak, failed ordinary publication, or unambiguous consumer contradiction under an existing invariant. The demonstrable downside is surprising metadata from an author-provided unusual combination; confidence/materiality is too low to justify the proposed cross-layer migration.

No new recommendation. Preserve the report and this disposition for fresh audit-of-audit; revisit only with a concrete format contract or stronger observed defect. A future bounded canonical-MIME rule would be smaller than the proposed broad representation rewrite, but is not needed to complete this audit.

C06 source unchanged independently verified after wrapper final exit 0 (session21280), HEAD3ee0621/treea4aac830. No tests/builds/live rows examined.
