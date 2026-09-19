# False-close telemetry

NEEDLE records `bead.false_close_detected` when a clean-extraction,
fallback, or claimed-evidence gate finds that an agent closed a bead and
reopens it. The event is emitted alongside `VerificationFailed` and carries
the bead, workspace, adapter, model, and one stable class label:

| Class | Meaning |
| --- | --- |
| `never_compiled` | The committed extraction did not compile or otherwise failed its build validation. |
| `uncommitted_dependency` | The dirty dispatch workspace depended on files or changes absent from the clean extraction. |
| `named_test_red` | A named test in the close evidence or configured gate failed. |
| `no_evidence` | The close had no usable verification evidence. |
| `deliverable_blocked` | The claimed deliverable was blocked, incomplete, or failed shipped-work verification. |

False closes use the same `failure-count:N` increment as a failed dispatch.
Consequently, repeated false closes reach the existing ADR-012 quarantine
threshold and stop the affected work from being handed out normally.

`needle status` includes a **False Closes by Adapter** table with adapter,
model, and count. JSON status exposes the same rows as
`false_close_counts`.
