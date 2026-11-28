# Store recovery

Quiesce new writes through the serving owner before changing production routing. Keep the source, its raw MANIFEST or head, journal END records, claim and any externally acknowledged positions. Put operation receipts and verifier output on a separate volume. Record the StoreId and the first failed operation, including its file or key and offset when available.

| Case | Evidence and action |
| --- | --- |
| R0. Writer holds LOCK or destination has EXPORT | For a served group, run `hirsel store backup --state STATE --group GROUP BACKUP`. For a stopped Directory, use `hirsel store backup DIR BACKUP`. Exit 75 means a writer holds the store. Keep a stamped backup read-only. For an interrupted restore, look up the original operation and resume its approved attempt. Leave its fence in place. |
| R1. Fenced writer, uncertain publication or held stream slot | Read the operation with `hirsel op get OP --json`. Reconcile the current head, claim, epoch and acknowledged positions before admitting traffic. Match each outstanding call against its caller's operation or idempotency key. Preserve uncertainty if the evidence cannot identify the winner. |
| R2. Journal or END damage | Run `hirsel store verify COPY` on an isolated copy. Compare the first bad offset with the terminal END, the successor's bound and each external durable acknowledgement. A torn tail above a valid bound can recover. Damage inside a covered prefix or a missing END requires an independently verified boundary. Record the D6 case when the external ledger exceeds the recoverable prefix. Keep the original END. |
| R3. Pack, blob, MANIFEST or checkpoint damage | Save the verification fault. Inspect the backup or retained archive intervals. Restore a boundary whose entire graph verifies into an empty destination. Record any unprotected suffix. Opening with creation enabled on the damaged path can destroy evidence. |
| R4. Incorrect derived index | Check the copy with `hirsel store verify COPY --release EXE`. After physical verification succeeds, run `hirsel store repair STOPPED_COPY --release EXE` on a separate stopped copy. Repeat the release-backed check and compare the primary rows against preserved evidence before considering promotion. |
| R5. Bad logical write | Select the last good explicit branch version, or inspect the physical boundary returned for a time selector. Equal timestamps resolve in physical order. Compare the restored business rows with a trusted ledger. A verified restore still needs separate host admission and routing authority. |
| R6. Missing object | Re-read the archive binding, head, intervals and floor. A target below a newly advanced floor was reclaimed. A missing object still promised by the witnessed head is loss. Preserve that head, quarantine the affected interval, correct lifecycle deletion and obtain an independent healthy backup or a new verified base. |
| R7. Gap, lag, failed drill or ENOSPC | Record the last certified boundary and the open gap. Relieve pressure on the affected resource and restore sink access. After a poisoned write, reopen and verify a preserved copy. A new base begins a new interval. Drill it before advertising coverage. |

## Drill a backup or archive

For a stamped backup, set `SOURCE` to its directory. For an archive, use a descriptor with the parent StoreId and the exact object prefix, including its trailing slash.

```json
{"endpoint":"https://s3.example.org","region":"us-east-1","bucket":"recovery","prefix":"west/","parent":"5ecfd92b020e2a1e0a6d81c54c6fdbda"}
```

Set `SOURCE=@/absolute/path/archive.json`. Supply the reader's `HIRSEL_ARCHIVE_READ_ACCESS_KEY_ID` and `HIRSEL_ARCHIVE_READ_SECRET_ACCESS_KEY` in the environment, plus `HIRSEL_ARCHIVE_READ_SESSION_TOKEN` for temporary credentials. The drill requires read-only credentials.

```sh
hirsel store archive describe "$SOURCE" --json > "$EVIDENCE/intervals.json"
hirsel store archive verify "$SOURCE" --deep --json > "$EVIDENCE/graphs.json"
```

Keep the live source offline from selection through verification. Set `XDG_STATE_HOME` and `TMPDIR` to directories outside both stores. Keep the release executable and its identity with the external evidence.

```sh
hirsel store restore --from "$SOURCE" --target "version:main,$VERSION" --to "$CHILD" --plan --json > "$EVIDENCE/plan.json"
jq -e '.exit == 0 and .outcome.effect == "none"' "$EVIDENCE/plan.json"
OP=$(jq -er '.operation.id' "$EVIDENCE/plan.json")
DIGEST=$(jq -er '.result.plan_digest' "$EVIDENCE/plan.json")
```

Review the destination, parent, release binding, selected version and physical boundary. After approval, record the reviewed digest in a typed decision.

```sh
printf 'host_v1.Decision(plan_digest: "%s", verdict: Approve, entries: [])\n' "$DIGEST" > "$EVIDENCE/decision.hcfg"
hirsel op decide "$OP" --decision "$EVIDENCE/decision.hcfg" --json > "$EVIDENCE/restore.json"
hirsel store verify "$CHILD" --release "$EXE" > "$EVIDENCE/release-verify.txt"
```

If publication loses its reply, use `hirsel op get "$OP" --json`. Resume with the original decision if the operation still needs publication. A new plan allocates another attempt. Save the actual exit of each command. Preserve the receipt, the verifier identity and the exact data compared. Without independent source attestation, report “restored and verified; source equality unmeasured.”

## Adopt a verified child

For a Directory child, prepare a `host_v1.Group` with a fresh name and listener. Set its store to the receipt's absolute destination. Give its workdir and Dir grant access to that same Directory. Pin the host context that retains the original assembly. Keep the receipt and each decision outside both stores.

```sh
hirsel host adopt "@$EVIDENCE/restore.json" --as "$GROUP" --context "$CONTEXT" --plan --json > "$EVIDENCE/adoption.json"
OP=$(jq -er '.operation.id' "$EVIDENCE/adoption.json")
hirsel op decide "$OP" --context "$CONTEXT" --decision "$EVIDENCE/create.hcfg" --wait --json > "$EVIDENCE/admission.json"
```

Review the creation plan's child, parent, destination, release binding and attempt against your saved receipt before writing `create.hcfg`. If admission requests another decision, check its assembly, requested grants and plan digest and submit that decision to the same operation. Use `--assembly HEX` to select among retained assemblies with the same image or to bootstrap an unactivated Store. The launcher checks the immutable assembly and the stopped child's recorded provenance. The host records the expected child identity and waits for the Place to open that Store before routing clients.

Read `hirsel status --group "$NEW_NAME@$NEW_INCARNATION" --context "$CONTEXT" --json` and compare its StoreId to the receipt. Exercise a service write, take a second backup through the serving Place and reopen a copy to check the written data. Keep the old incarnation and its evidence until these checks pass.

## Advance the retention floor

Run `hirsel store archive expire "$SOURCE" --keep-from "$BASE" --json` with the reader role. Check the old and proposed floors and the retained interval against the required recovery window. Approve the reviewed digest using the same decision form as restore. The apply process uses `HIRSEL_ARCHIVE_EXPIRY_ACCESS_KEY_ID`, `HIRSEL_ARCHIVE_EXPIRY_SECRET_ACCESS_KEY` and, for temporary credentials, `HIRSEL_ARCHIVE_EXPIRY_SESSION_TOKEN`. Save its reply externally. If the reviewed head changed, inspect the archive and prepare another plan. After a lost reply, a newer floor alone cannot identify which operation advanced it.

## Restore to a standalone Bucket

Put the destination profile in an operator-owned file under a trusted directory. Give the destination its own empty prefix and write credential. Supply the source Place and host stream coordinates from deployment configuration. Use JSON null for a namespace that is absent.

```json
{"location":{"endpoint":"https://objects.example.org","region":"auto","bucket":"recovery","prefix":"children/drill/"},"level":"bucket","source_place":null,"host_stream":null}
```

Set `HIRSEL_RESTORE_BUCKET_ACCESS_KEY_ID` and `HIRSEL_RESTORE_BUCKET_SECRET_ACCESS_KEY` for that prefix. Temporary credentials also need `HIRSEL_RESTORE_BUCKET_SESSION_TOKEN`. Use `--to @/absolute/path/to/bucket.json` with the same plan and decision commands. If the commit reply is lost, `op get` can identify the published child from the destination with its own credential. The process launcher currently admits Directory children. A streamed Bucket also requires host-owned credentials, a slot for the child, conditional ATTACH and acknowledgement of its current claim.

## Retire the old incarnation

Read the old incarnation from the pinned host. After moving clients and taking a final backup, review the retirement plan's predecessor and assembly. Record this plan's digest in a new typed decision. The host keeps the workdir. Wait for the phase `retired` before reusing the name or listener.

```sh
hirsel host retire --group "$OLD_NAME@$OLD_INCARNATION" --context "$CONTEXT" --plan --json > "$EVIDENCE/retirement.json"
OP=$(jq -er '.operation.id' "$EVIDENCE/retirement.json")
hirsel op decide "$OP" --context "$CONTEXT" --decision "$EVIDENCE/retire.hcfg" --wait --json
```
