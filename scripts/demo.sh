#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
bin="${CLAUCK_BIN:-$here/target/release/clauck}"
if [[ ! -x "$bin" ]]; then
  echo "Building clauck..."
  cargo build --release --manifest-path "$here/Cargo.toml" >/dev/null
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/clauck-demo.XXXXXX")"
trap 'rm -rf "$work"' EXIT
project="$work/demo project (copy)"
cp -R "$here/examples/demo-project" "$project"
export CLAUCK_HOME="$work/store"
export CLAUDE_CONFIG_DIR="$work/claude"

step() { printf '\n\033[1m$ %s\033[0m\n' "$*"; }
run() { step "clauck $*"; "$bin" --path "$project" "$@"; }

run status
run lock --dry-run
run lock
step "cat AGENTS.md"; cat "$project/AGENTS.md"
step "cat .clauck/policy.md"; cat "$project/.clauck/policy.md"
run status

step "# Manual work is unaffected while locked"
python3 "$project/tests/test_calculator.py"
sed -i.bak 's/return a + b/return b + a/' "$project/src/calculator.py" && rm "$project/src/calculator.py.bak"
python3 "$project/tests/test_calculator.py"

step "# Password mode (passwords via --password-stdin only for this scripted demo)"
printf 'demo-pass\ndemo-pass\n' | "$bin" --path "$project" lock --password --password-stdin
step "clauck unlock   # no terminal, so it refuses instead of hanging"
"$bin" --path "$project" unlock || echo "(exit $?: authentication required)"
step "clauck unlock (wrong password)"
printf 'wrong\n' | "$bin" --path "$project" unlock --password-stdin || echo "(exit $?: still locked)"
printf 'demo-pass\n' | "$bin" --path "$project" unlock --password-stdin

run doctor --skip-agent-versions
printf 'demo-pass\n' | "$bin" --path "$project" uninstall --password-stdin
step "diff original AGENTS.md against the uninstalled copy"
diff "$here/examples/demo-project/AGENTS.md" "$project/AGENTS.md" && echo "identical"
