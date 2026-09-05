# Sanitizer memory reduction

On 2026-09-05, the live NEEDLE coordinators on codinghome commonly held about
490 MiB each in their main heap. This was separate from the coding harnesses,
build tools, filesystem cache, and kernel memory charged to `needle.slice`.

An isolated release-mode probe importing the production sanitizer found that
compiling the vendored rules accounted for most of that footprint:

| Measurement | Before | After |
|---|---:|---:|
| Live Rust allocations after sanitizer construction | 439.59 MiB | 15.96 MiB |
| Peak live Rust allocations during construction | 446.07 MiB | 16.46 MiB |
| Probe process RSS after construction | about 478 MiB | about 23 MiB |
| Construction time | 1,825 ms | 76 ms |
| Successfully compiled content rules | 218 | 221 |

The probe counted allocations through `std::alloc::System` and read Linux
process memory accounting. These are isolated component measurements, not a
post-deployment fleet measurement. Allocation tracking was disabled when
running latency tests, because its atomics add overhead to allocation-heavy
code. The approximately 424 MiB saving per sanitizer would remove about
7.4 GiB of allocated heap across 18 workers with one sanitizer each.

## Cause and change

Gitleaks uses Go's regex semantics. Its Perl classes (`\w`, `\d`, `\s` and
their complements) and word boundaries are ASCII-based. Rust regex defaults
to Unicode for those constructs. In patterns containing repeated `\w`
classes, that expands the compiled automata enormously. Three content rules
even exceeded Rust regex's compilation limit and were skipped.

The import path now converts those constructs to their Go equivalents,
including in allowlists. It preserves escaped backslashes and leaves Unicode
dots, negated classes, properties, and case folding enabled. Custom workspace
patterns keep their existing Rust regex semantics. All 221 vendored content
rules compile; the path-only rule is still intentionally excluded.

The line sanitizer also computes lowercase keyword-filter text once per
changed line and borrows unchanged input between rules. This removes repeated
copies of lines for rules that never redact anything.

References: [Go regex syntax](https://pkg.go.dev/regexp/syntax),
[Rust regex Unicode behavior](https://docs.rs/regex/latest/regex/struct.RegexBuilder.html#method.unicode).

## Regression checks

```bash
cargo test --lib sanitize::tests -- --test-threads=2
```

The Linux memory test starts a child copy of the test executable running only
the sanitizer check. This isolates its peak RSS from other tests and allocator
reuse; it does not start a worker or access bead stores. The allowed startup
increase is 64 MiB. The original implementation fails that check with an
approximately 480 MiB increase.

Other tests require every vendored content rule to compile and cover ASCII
classes and boundaries, retained Unicode behavior, literal escapes, capture
groups, allowlists, custom patterns, and keywords introduced by earlier
redactions. The representative regex expectations were also checked against
Go's actual `regexp` implementation.
