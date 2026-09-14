# Library Test Process-Site Inventory

This catalog is the checked inventory for Cargo's compiled `--lib` test
target. It deliberately covers unit-test code only. Production process
boundaries and integration targets are outside this inventory: the former are
allowed implementation seams, while real-process behavioral contracts belong
in `tests/integration_spawn`.

The executable contract at `tests/lib-process-purity/run.sh` derives the
inventory below with the repository's Rust-aware test-policy scanner and fails
if this document drifts. It then runs the complete library test executable at
default parallelism under `strace`, accepting only the executable's initial
bootstrap `execve` and rejecting every later `execve` or `execveat` attempt,
including failed attempts. There is no process allowlist and no skipped-test
exception.

## Direct process constructors in `#[cfg(test)]`

<!-- lib-process-purity-inventory:start -->
(none)
<!-- lib-process-purity-inventory:end -->

This empty static inventory and the dynamic zero-child assertion complement
one another. The static check catches dormant or filtered raw constructor
sites; the dynamic check catches indirect execution through production helpers
or injected adapters. Run both with:

```text
bash tests/lib-process-purity/run.sh
```

The script prints the live library test count from the test executable instead
of hard-coding it here, so adding a pure unit test does not create false catalog
drift.
