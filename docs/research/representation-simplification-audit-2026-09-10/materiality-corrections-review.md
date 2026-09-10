# Materiality corrections review

**REVISE — two requested qualifications remain unaddressed in the canonical report.**

1. `report.md:25` still assigns F08 **Medium** priority. Change this row to **Low**, matching the materiality verdict and revised F08 note. The other four required low-priority rows (F12/F15/F21/F22) are correct. Reconciliation currently overstates completion of this correction.
2. `report.md:27` still says “preserve device-scoped registration.” Replace this with preservation of the existing **history-independent, token-keyed registry** across reset. The revised F18 note correctly states that there is no device key and that registration/preservation does not implement per-device token replacement or revocation; the canonical summary must agree.

The affected notes otherwise satisfy the bounded qualifications: F17 distinguishes app callback/destination failures from all Android notification paths; F18 preserves local off behavior and latest-token registration; F21 identifies the unbounded map and immutable rename/replay input; F09 specifies conditional tool input, validated host/storage reads, NULL-safe SQL mapping and no regex performance claim; F14 fixes latest-upsert ordering with its O(n) tradeoff; F06 separates nosniff/allowlist policy from byte validation; F20 documents shared NAT/proxy throttling. F22 remains exactly two retired files and historical-document cleanup.

Ownership verification: all 470 file rows match their default owners and owner-cluster statuses (360 recommend, 110 skip). All 27 clusters are final (17 recommend, 10 skip). Coverage review is PASS and the canonical report records it.

Source integrity before and after: HEAD `3ee0621a603659ab0168f565b99012b642415419`; tree `a4aac830c45398a66591f2c44b707aaf3cef281b`; porcelain status empty. Only this external deliverable was written. No source edits, tests, builds, network/live reads, commits or delegation. This verdict concerns audit-document corrections only, not later implementation.
