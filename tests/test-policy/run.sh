#!/usr/bin/env bash
# Mutation tests for scripts/check-test-policy.sh.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHECKER="$REPO_ROOT/scripts/check-test-policy.sh"
FIXTURE="$REPO_ROOT/tests/test-policy/fixtures/valid"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-test-policy-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

passes=0
failures=0

ok() {
  echo "PASS: $1"
  passes=$((passes + 1))
}

bad() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

fresh_fixture() {
  local destination="$1"
  mkdir -p "$destination"
  cp -R "$FIXTURE/." "$destination/"
}

expect_failure() {
  local name="$1" expected="$2" root="$3" output
  if output="$(NEEDLE_TEST_POLICY_ROOT="$root" \
      NEEDLE_TEST_POLICY_FILE=tests/test-policy.toml \
      "$CHECKER" 2>&1)"; then
    bad "$name was accepted"
  elif grep -Fq -- "$expected" <<<"$output"; then
    ok "$name"
  else
    bad "$name failed without '$expected': $output"
  fi
}

valid="$TMP_ROOT/valid"
fresh_fixture "$valid"
if NEEDLE_TEST_POLICY_ROOT="$valid" \
    NEEDLE_TEST_POLICY_FILE=tests/test-policy.toml "$CHECKER" >/dev/null; then
  ok "valid classified fixture"
else
  bad "valid classified fixture"
fi

case_root="$TMP_ROOT/tier"
fresh_fixture "$case_root"
sed -i 's/tier = "contract"/tier = "nightly"/' "$case_root/tests/test-policy.toml"
expect_failure "unknown harness tier is rejected" "invalid or missing tier" "$case_root"

case_root="$TMP_ROOT/missing-harness"
fresh_fixture "$case_root"
printf '\n[[test]]\nname = "stray"\npath = "tests/stray.rs"\n' \
  >>"$case_root/Cargo.toml"
printf '#[test]\nfn stray() {}\n' >"$case_root/tests/stray.rs"
expect_failure "unclassified Cargo harness is rejected" \
  "Cargo integration harness has no test tier: stray" "$case_root"

case_root="$TMP_ROOT/loose-rust"
fresh_fixture "$case_root"
printf '#[test]\nfn invisible_to_cargo() {}\n' >"$case_root/tests/loose.rs"
expect_failure "unclassified top-level Rust file is rejected" \
  "unclassified top-level Rust test: tests/loose.rs" "$case_root"

case_root="$TMP_ROOT/growth"
fresh_fixture "$case_root"
printf '\n#[test]\nfn exceeds_budget() {}\n' >>"$case_root/tests/contract_tests/basic.rs"
expect_failure "harness growth is rejected" \
  "harness contract_tests exceeds growth budget: 2 > 1" "$case_root"

for hazard in sleep process global-env; do
  case_root="$TMP_ROOT/$hazard"
  fresh_fixture "$case_root"
  case "$hazard" in
    sleep)
      body='std::thread::sleep(std::time::Duration::from_millis(1));'
      ;;
    process)
      body='let _command = std::process::Command::new("true");'
      ;;
    global-env)
      body='std::env::set_var("NEEDLE_POLICY_FIXTURE", "1");'
      ;;
  esac
  printf '\n#[cfg(test)]\nmod policy_bad {\n  #[test]\n  fn hazard() { %s }\n}\n' "$body" \
    >>"$case_root/src/lib.rs"
  # The extra test also exceeds the unit count, but the hazard diagnostic must
  # still be aggregated and named; the checker never stops at the first error.
  expect_failure "raw unit-test $hazard is rejected" \
    "unit tier uses raw $hazard without an owned exception" "$case_root"
done

case_root="$TMP_ROOT/unowned"
fresh_fixture "$case_root"
printf 'verification data\n' >"$case_root/tests/orphan.snapshot"
expect_failure "unowned verification artifact is rejected" \
  "verification-only test artifact has no owner: tests/orphan.snapshot" "$case_root"

echo "test-policy checker: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
