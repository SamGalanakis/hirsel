# Hirsel combined audit inventory

Fixed audit HEAD `3ee0621a603659ab0168f565b99012b642415419`, tree `a4aac830c45398a66591f2c44b707aaf3cef281b`. Source remains read only.

Every path has one default owner. Exact named top-level definitions override that default; no symbol has two owners. Consumers may inspect across boundaries but report only owned representation/behavior.

Coverage: 557 tracked paths; 470 source/test/tooling paths owned; 87 explicit noncode context exclusions; 27 clusters. States: {'skip': 10, 'recommend': 17}. Zero uncovered paths.

Exact owned files/definitions and verified anchors, existing tests, consumer paths and subsystem crosswalk are in `ownership.json`. Consumers may read other layers but never become co-owners.

| ID | Owned files | Shared definitions | Status | Evidence |
|---|---:|---:|---|---|
| C01-THREAD-IDENTITY | 11 | 7 | skip | workers/C01-THREAD-IDENTITY.md; verified/C01-F01-disposition.md |
| C02-TURN-LIFECYCLE | 20 | 14 | recommend | workers/C02-TURN-LIFECYCLE.md; verified/F01-cli-terminal-fifo.md; verified/C02-01-disposition.md |
| C03-CONVERSATION | 4 | 6 | skip | workers/C03-CONVERSATION.md; verified/C03-dispositions.md |
| C04-BLOBS | 5 | 4 | recommend | workers/C04-BLOBS.md; verified/F06-blob-inline-policy.md; verified/F07-blob-location-ownership.md |
| C05-DELEGATION-SCOPE | 13 | 4 | skip | workers/C05-DELEGATION-SCOPE.md; verified/C05-F01-disposition.md |
| C06-ARTIFACTS | 7 | 7 | skip | workers/C06-ARTIFACTS.md; verified/C06-F1-disposition.md |
| C07-RELATED | 3 | 4 | skip | workers/C07-RELATED.md; verified/C07-F01-disposition.md |
| C08-PROCESS-WAKE | 19 | 1 | recommend | workers/C08-PROCESS-WAKE.md; verified/F08-monitor-activity-label.md; verified/C08-01-disposition.md |
| C09-CODEX-DRIVER | 6 | 0 | skip | workers/C09-CODEX-DRIVER.md; verified/C09-dispositions.md |
| C10-CLAUDE-DRIVER | 5 | 0 | skip | workers/C10-CLAUDE-DRIVER.md; verified/C10-dispositions.md |
| C11-DRIVER-SHARED | 9 | 0 | recommend | workers/C11-DRIVER-SHARED.md; verified/F10-shell-timeout-stderr.md |
| C12-LASH-RUNTIME | 8 | 0 | recommend | workers/C12-LASH-RUNTIME.md; verified/F09-monitor-condition-validation.md |
| C13-CONFIG | 15 | 2 | recommend | workers/C13-CONFIG.md; verified/F11-provider-file-validation.md; verified/C13-02-disposition.md |
| C14-PROTOCOL-CONNECTION | 27 | 66 | recommend | workers/C14-PROTOCOL-CONNECTION.md; verified/F03-view-removal-dedupe.md; verified/F04-reconnect-auth-phase.md |
| C15-WEB-THREADS | 46 | 0 | recommend | workers/C15-WEB-THREADS.md; verified/F05-action-error-ownership.md; root #13/#12 regressions deduped |
| C16-WEB-ARTIFACTS | 25 | 0 | skip | workers/C16-WEB-ARTIFACTS.md; verified/C16-dispositions.md |
| C17-WEB-RELATED | 12 | 0 | skip | workers/C17-WEB-RELATED.md; verified/C17-skip.md |
| C18-WEB-VIEWS | 7 | 0 | recommend | workers/C18-WEB-VIEWS.md; verified/F13-canvas-only-view-contract.md; verified/F14-view-order-ownership.md |
| C19-WEB-SETTINGS | 21 | 0 | recommend | workers/C19-WEB-SETTINGS.md; verified/F15-dead-debug-setting.md; verified/F16-provider-write-latch.md |
| C20-WEB-SHELL | 60 | 0 | recommend | workers/C20-WEB-SHELL.md; verified/F12-pane-header-saving.md; verified/C20-F2-disposition.md |
| C21-CLIENT-CORE | 13 | 0 | recommend | workers/C21-CLIENT-CORE.md; verified/F02-thread-action-history.md; verified/C21-F2-disposition.md |
| C22-FFI-ANDROID | 31 | 0 | recommend | verified/F17-push-history-projection.md; verified/F18-push-registration-lifecycle.md |
| C23-PLUGINS | 24 | 4 | skip | verified/C23-dispositions.md |
| C24-VIEWS-INSTRUMENTS | 16 | 0 | recommend | verified/F19-form-field-identity.md; verified/C24-02-disposition.md |
| C25-HOST-OPS | 19 | 4 | recommend | verified/F18-push-registration-lifecycle.md; verified/F20-websocket-peer-throttle.md |
| C26-BUILD-TOOLING | 31 | 0 | recommend | verified/F23-android-build-ownership.md; verified/C26-01-disposition.md |
| C27-TEST-INFRASTRUCTURE | 13 | 0 | recommend | verified/F21-mock-thread-projection.md; verified/F22-retired-task-harness.md |

## Boundary corrections

- `storage/threads.rs` uses actual `from_row`, `get`, `snapshot`, `attention`, `validate_instrument`, and Storage thread/create_thread/update_thread/settle_thread/archive_thread/snooze_thread/pin_thread/mark_thread_read methods. Invented ThreadRow/create/update/pin anchors were removed.
- `cli_turn.rs` is C02 only. Web `threads/store.ts` and `ThreadMessages.tsx` are C15 only. Native core C21 and FFI/Android C22 own their complete local representations.
- `storage/chat.rs` is C03 (conversation); blob loading there is a consumer conversion read for C04. SQL/proto shared top-level definition overrides explicitly name one owner.
- Unit tests are assigned to their implementation domain; C27 owns cross-system browser/e2e/mock harness infrastructure, C26 owns build/static/publication machinery.

## Context-only exclusions

Design mockups, product/research documentation, frozen prior research probes and licenses are enumerated in ownership.json. They provide constraints but do not substitute for source coverage.

## Audit log

- Read invoked skills, wholehog steering, global/repository instructions and final PRODUCT/ADRs.
- Corrected overlapping provisional ownership, invented anchors and stale removed test references.
- Refreshed to final 3ee0621/tree a4aac830 and verified all 470 owned paths exist.
- Early F01 independently verified and tracked by root as #16; no source edits, tests, builds or live reads by audit.
