#!/usr/bin/env python3
"""Prepare an isolated copy without modifying the supplied Hirsel checkout."""
import pathlib
import shutil
import sys

repo = pathlib.Path(sys.argv[1]).resolve()
output = pathlib.Path(sys.argv[2]).resolve()
assets = pathlib.Path(__file__).resolve().parent
if output.exists():
    raise SystemExit(f"Refusing to overwrite existing directory: {output}")
output.mkdir(parents=True)
shutil.copytree(repo / "crates/hirsel-drivers/src", output / "src")
shutil.copy2(assets / "Cargo.toml", output / "Cargo.toml")
shutil.copytree(assets / "bin", output / "bin")
(output / "examples").mkdir()
shutil.copy2(assets / "examples/verify.rs", output / "examples/verify.rs")
source = (repo / "crates/hirsel-host/src/tools.rs").read_text()
start = source.index("#[derive(Clone)]\nstruct TerminalEventBus")
end = source.index("\n#[derive(Debug, Clone, Serialize)]\npub struct SpawnedProcess", start)
original = source[start:end]
template = (assets / "examples/verify_bus.rs.in").read_text()
(output / "examples/verify_bus.rs").write_text(
    template.replace("// INSERT_CURRENT_TERMINAL_BUS_HERE", original)
)
print(output)
