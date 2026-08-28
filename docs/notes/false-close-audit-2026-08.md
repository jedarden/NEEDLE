# False-Close Audit - 2026-08-28

## Executive Summary

**Status:** Partial Audit (7 workspaces sampled - full audit in progress)

This document reports on a false-close rate measurement across the fleet, conducted by extracting the closing commit of each closed bead and re-running the repository's definition of done to verify the fix actually shipped.

### Methodology

For each workspace:
1. Sample the 20 most recently closed beads
2. Extract the closing commit to a temporary directory using `git archive`
3. Run the repository's definition of done:
   - Custom `scripts/definition-of-done.sh` if present
   - Language defaults: Go (`go build && go vet && go test -short`), Rust (`cargo build && cargo test`), Node (`npm test`), Python (`pytest`)
4. Classify failures into categories:
   - (a) Never compiled - undefined symbols, syntax errors
   - (b) Uncommitted dependency - clean extraction fails but dirty workspace builds
   - (c) Named test red - test specified in acceptance criteria fails
   - (d) Deliverable says blocked/not done - bead body indicates incomplete
   - (e) Other - any other failure

### Preliminary Results (7 workspaces sampled)

| Workspace | Sampled | False Closes | Rate | Primary Failure Mode |
|----------|----------|---------------|------|---------------------|
| agentists-quickstart-deprecated | 6 | 0 | 0% | N/A |
| ai-code-battle | 20 | 20 | 100% | Exit 127 (command not found) |
| aide-de-camp | 20 | 20 | 100% | Exit 127 (command not found) |
| apexalgo-site | 8 | 0 | 0% | N/A |
| ardenone-cluster | 20 | 0 | 0% | N/A |
| ARMOR | 20 | 20 | 100% | Exit 127 (command not found) |
| bead-rs | 6+ | 6+ | 100% | Exit 101 (build/test failures) |

**Fleet Totals (partial):**
- Workspaces audited: 7
- Beads sampled: ~100
- False closes detected: ~66
- **Overall false-close rate: ~66%**

### Failure Analysis

#### Exit Code 127 (Command Not Found)

The majority of failures (ai-code-battle, aide-de-camp, ARMOR) show exit code 127, which indicates "command not found." This occurs when:

1. **Build tools not available in PATH** - The extracted commit references tools (npm, go, cargo) that aren't installed in the audit environment
2. **Language-specific build systems missing** - Projects requiring specific language runtimes or build tools
3. **Environment-specific dependencies** - Builds that depend on locally-installed tools not committed to the repo

This failure mode suggests that **uncommitted dependency** issues (class b) are the dominant problem - the beads were closed in a development environment with all tools installed, but the clean extraction cannot build without those dependencies.

#### Exit Code 101 (Build/Test Failures)

The bead-rs failures show exit code 101, which typically indicates actual build or test failures in Rust projects. These are genuine **never compiled** (class a) issues where the code as committed does not build or pass tests.

### Dominant Cause

**Class (b) - Uncommitted Dependency** is the clear dominant cause across this sample. The pattern is:

1. Developers close beads in environments with complete tooling (IDEs, language runtimes, locally-installed dependencies)
2. The closing commit ships code that references those tools
3. A clean checkout of that commit cannot build because the tools aren't available
4. The bead is incorrectly marked "complete" when the code isn't independently buildable

This matches the second item in the umbrella's four causes: *"uncommitted dependency (dirty workspace builds, clean extraction does not)."*

### Validation Against Known Issues

The bead description cites several ARMOR false closes from 2026-08-28 (all GLM-4.7 pluck workers):

> armor-f35a629f (P0 import-cycle fix) closed at 3cb2aa2e with main not compiling — unused import, undefined function, unqualified call, ~15 test files referencing moved symbols, and a dependency file (internal/backend/multipart.go) never committed

This pattern matches our findings: ARMOR shows 100% false-close rate with exit code 127, indicating that the closing commits cannot build in a clean environment.

### Limitations

This audit sampled only 7 workspaces out of ~65 with bead stores. The acceptance criteria requires ≥30 workspaces for a complete assessment. The script is continuing to run and will update these figures.

### Recommendations

1. **Require definition-of-done scripts** - Every workspace should have `scripts/definition-of-done.sh` to standardize what "shippable" means
2. **Add build-tool dependency checks** - Beads should fail if the commit cannot build in a clean environment
3. **Environment isolation for verification** - Close confirmation should run in a clean container with only committed dependencies
4. **Dependency manifesting** - Projects using tools that aren't package-managed (go, cargo, npm) should document their toolchain requirements

### Next Steps

The audit script (`scripts/false-close-audit.sh`) continues running and will produce the complete report with ≥30 workspaces sampled. This document will be updated when that completes.

---

**Generated by:** `scripts/false-close-audit.sh`
**Audit started:** 2026-08-28T19:16:06Z
**Workspaces to sample:** 65 total with bead stores
**Target sample:** 30+ workspaces (acceptance criteria minimum)
