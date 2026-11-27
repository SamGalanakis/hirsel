#!/bin/sh
# dev_fixture.sh LIBRARIES TESTDATA WORKSPACE: lay out the dev_laws workspace
# (runner with dev_place.hirsel as its program, and store) from LIBRARIES (the
# root holding programs/runner and programs/store) into WORKSPACE, with the
# library root WORKSPACE/libraries the builds pass as --libraries. The dev_laws
# Fixture and the //:dev-fixture-cache action both lay it out with this script,
# so the action's build of it seeds the objects the dev child's first build asks for.
set -eu
libraries="$1"; testdata="$2"; workspace="$3"
for package in runner store; do
    mkdir -p "$workspace/$package"
    for file in "$libraries/programs/$package"/hirsel.toml "$libraries/programs/$package"/*.hirsel; do
        cp -L "$file" "$workspace/$package/"
    done
done
# The runner manifest's test sources outside its root: a build checks they exist.
mkdir -p "$workspace/runner/tests" "$workspace/runner/observe"
cp -L "$libraries/programs/runner/tests/b9-vectors.hirsel" "$workspace/runner/tests/"
cp -L "$libraries/programs/runner/observe"/*.hirsel "$workspace/runner/observe/"
cp -L "$testdata/dev_place.hirsel" "$workspace/runner/"
printf '[workspace]\nmembers = ["runner", "store"]\n' > "$workspace/hirsel.toml"
# The library root the dev child is given: core and std from LIBRARIES, and
# the members as its programs, so the runner is the toolchain's (launch.md,
# "Private prefix") and the build gets the place bit.
mkdir -p "$workspace/libraries/programs"
ln -s "$libraries/core" "$workspace/libraries/core"
ln -s "$libraries/std" "$workspace/libraries/std"
ln -s ../../runner "$workspace/libraries/programs/runner"
ln -s ../../store "$workspace/libraries/programs/store"
# The copies keep the inputs' read-only mode under remote execution: replace
# the manifest rather than write through it.
manifest="$workspace/runner/hirsel.toml"
sed '/^\[sources\]$/a\
"dev_place.hirsel" = "runner"
' "$manifest" > "$manifest.new"
printf '\n[program]\nmodule = "runner"\nfunction = "dev_fixture"\n' >> "$manifest.new"
rm -f "$manifest"; mv "$manifest.new" "$manifest"
chmod -R u+w "$workspace"
