"""Launch the Fly preview writer, discovery projection and ingress."""
import base64
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time

WRITER = 11001
DISCOVERY = 11002
WORKER = "--hirsel-compiler-worker=/opt/registry/hirsel"
BASE_ENV = {"PATH": os.defpath, "LANG": "C.UTF-8"}


def private_dir(path, uid):
    path.mkdir(mode=0o700, exist_ok=True)
    os.chown(path, uid, uid)
    path.chmod(0o700)


def signer(name, variable):
    # A FIFO transfers the DER bytes from the Fly secret directly to the
    # runtime's file grant. Its directory entry never holds key material.
    path = Path("/run/registry-keys") / name
    path.unlink(missing_ok=True)
    os.mkfifo(path, 0o600)
    os.chown(path, WRITER, WRITER)
    key = base64.b64decode(os.environ.pop(variable), validate=True)

    def deliver():
        with path.open("wb") as pipe:
            pipe.write(key)
        path.unlink()

    threading.Thread(target=deliver, daemon=True).start()


def launch(name, uid, args, extra=None):
    work = Path("/tmp") / ("registry-" + name)
    private_dir(work, uid)
    env = BASE_ENV | {"HOME": str(work)} | (extra or {})
    group = compiler_group(name, uid)
    (group / "cgroup.procs").write_text("0")
    try:
        return subprocess.Popen(args, env=env, cwd=work, user=uid, group=uid, extra_groups=[])
    finally:
        Path("/sys/fs/cgroup/cgroup.procs").write_text("0")


def prepare_controllers():
    root = Path("/sys/fs/cgroup")
    if not (root / "cgroup.controllers").is_file():
        unified = root / "unified"
        needed = {"memory", "pids"}
        missing = needed - set((unified / "cgroup.controllers").read_text().split())
        # On a single-container Fly Machine, init mounts empty v1 hierarchies
        # at boot. Refuse to migrate any hierarchy carrying per-service limits.
        for name in sorted(missing):
            mount = root / name
            if not mount.is_mount() or any(p.is_dir() for p in mount.iterdir()):
                raise RuntimeError(f"expected an unused guest {name} hierarchy")
            if name == "memory":
                for limit in ["memory.limit_in_bytes", "memory.memsw.limit_in_bytes"]:
                    value = mount / limit
                    if value.exists() and value.read_text().strip() != "9223372036854771712":
                        raise RuntimeError("guest v1 memory hierarchy has a limit")
            if name == "pids" and (mount / "pids.max").exists():
                if (mount / "pids.max").read_text().strip() != "max":
                    raise RuntimeError("guest v1 PID hierarchy has a limit")
        for name in sorted(missing):
            subprocess.run(["umount", str(root / name)], check=True)
        end = time.monotonic() + 10
        while not needed <= set((unified / "cgroup.controllers").read_text().split()):
            if time.monotonic() >= end:
                raise RuntimeError("guest cgroup v2 controllers unavailable")
            time.sleep(0.1)
        os.unshare(os.CLONE_NEWNS)
        subprocess.run(["mount", "--make-rprivate", "/"], check=True)
        subprocess.run(["mount", "--bind", str(unified), str(root)], check=True)
    (root / "cgroup.subtree_control").write_text("+memory +pids")


def compiler_group(name, uid):
    root = Path("/sys/fs/cgroup")
    pool = root / ("registry-" + name)
    pool.mkdir(exist_ok=True)
    (pool / "cgroup.subtree_control").write_text("+memory +pids")
    leaf = pool / "service"
    leaf.mkdir(exist_ok=True)
    for entry in (pool, pool / "cgroup.procs", pool / "cgroup.threads", pool / "cgroup.subtree_control"):
        os.chown(entry, uid, uid)
    return leaf


def main():
    prepare_controllers()
    private_dir(Path("/run/registry-keys"), WRITER)
    private_dir(Path("/data/projection"), DISCOVERY)
    signer("log", "REGISTRY_LOG_DER")
    signer("status", "REGISTRY_STATUS_DER")
    services = {
        "writer": launch("writer", WRITER, ["/opt/registry/writer", WORKER],
                         {k: os.environ[k] for k in ("AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY")}),
        "discovery": launch("discovery", DISCOVERY, ["/opt/registry/discovery", WORKER]),
        "nginx": subprocess.Popen(["/usr/sbin/nginx", "-g", "daemon off;"], env=BASE_ENV),
    }
    def stop(*_):
        for service in services.values():
            service.terminate()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    pid, status = os.wait()
    name = next((n for n, p in services.items() if p.pid == pid), "child")
    print(f"{name} exited with {os.waitstatus_to_exitcode(status)}", file=sys.stderr)
    stop()
    return 1


if __name__ == "__main__":
    sys.exit(main())
