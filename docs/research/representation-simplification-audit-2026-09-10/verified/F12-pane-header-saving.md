# F12 — close control suppresses prompt saving status

Recommend; high confidence, medium/low priority. Owner C20. Worker F1 ../workers/C20-WEB-SHELL.md. Independently reopened PaneHeader props/rendering and complete expanded prompt editor plus PromptActions. Exact queries: 4 production PaneHeader consumers; the onClose/badge query returns 3 lines, of which 2 are the conflicting props on this header.

ExpandedPromptEditor supplies both onClose and a busy Saving loader via badge. PaneHeader chooses onClose with badge only as fallback, so every actual saving transition in that editor hides the requested loader; the remaining Save button/textarea simply disable. These are independent capabilities, not mutually exclusive variants.

Target: keep simple existing props, render badge and optional close button as siblings, and correct the stale non-dismissible-only badge comment. No new header configuration type or consumer rewrite. Scope PaneHeader.tsx plus a both-present DOM assertion preserving existing close-only/badge-only behavior. Risk only trailing-layout width; no wire/schema/data change. Audit ran no tests.

Independent materiality priority: low. This is a bounded display/wholehog cleanup correction, not peer severity to durable admission or captured identity defects.
