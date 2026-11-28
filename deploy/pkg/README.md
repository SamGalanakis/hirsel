# pkg.hirsel.ai preview

Fly terminates TLS. A single Machine runs the registry writer, the discovery service and nginx. The private Tigris bucket holds both logs and package objects. The volume at `/data` holds the rebuildable discovery projection. The public config pins the operator keys and selects zero witnesses. The enrollment document authorizes the publisher for `pkg.hirsel.ai`.

From the repository root, prepare Buck2 and build `//:hirsel-core-driver-bin-hirsel` and `//:core-driver-runtime-pic` with `--show-output`. Pass the resulting driver and archive paths to the build script. Use an empty image directory outside the checkout.

```sh
deploy/pkg/build.sh "$driver" "$runtime" "$image_dir"
(cd "$image_dir" && fly deploy --local-only --ha=false)
```

Fly attaches the bucket's `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` secrets. Supply the release and status signers through `REGISTRY_LOG_DER` and `REGISTRY_STATUS_DER` as base64-encoded Ed25519 PKCS#8 DER. The launcher delivers each signer to the writer through a private FIFO. The legal signer stays in operator custody and Fly secrets. Discovery receives a Compiler grant for the `source` tool and reads the public registry over HTTPS.

The single-container Fly guest starts with memory and PID controls on cgroup v1. The launcher checks that both guest hierarchies are empty and unlimited before moving their controllers to v2. It delegates a separate subtree to each service UID so the existing compiler worker can enforce its memory, swap and process limits. Fly continues to enforce the Machine's allocation outside the guest.
