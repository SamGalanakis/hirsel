#!/bin/bash
set -euo pipefail
unset SDKROOT DEVELOPER_DIR
guard=$(mktemp -d "${TMPDIR:-/tmp}/hirsel-no-xcrun.XXXXXX")
trap 'rm -rf "$guard"' EXIT
cat > "$guard/xcrun" <<'EOF'
#!/bin/sh
echo 'Hirsel image execution must not call xcrun' >&2
exit 127
EOF
chmod +x "$guard/xcrun"
export PATH="$guard:$PATH"
"$@"
