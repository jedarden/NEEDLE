# Claim verification routing

Dispatch keeps the store and claim identity captured during selection for the
entire attempt. A local bead uses the worker's home workspace as its resolved
target; a bead discovered in another workspace uses that workspace's store.
The dispatch-time verification and any conditional cleanup both use this same
target-store handle. The worker never reconstructs a store from a path, falls
back to the home store, or replaces the held revision/claim epoch with a later
read.

This preserves both routes:

- Local dispatch verifies and executes in the configured default workspace.
- Remote dispatch verifies and executes in the discovered bead workspace,
  including when the bead ID collides with a home-store bead.

If verification is unverifiable—such as a target lookup, parse, timeout, or
backend failure—the worker fails closed before spawning the agent. It attempts
conditional release only through the held target store and claim credential.
If that credential or target context is unavailable, or the claim has changed,
the claim is left untouched for lease expiry. The failure is structured as
`bead.claim.verify_error` (with `stage`, `target_workspace`, `category`, and
`detail`), while a skipped cleanup is `bead.claim.cleanup_skipped`.

Successful verification emits `bead.claim.verify_success` with the resolved
`workspace` and the captured `claim_epoch` when the backend provides fencing
epochs. These fields make local and remote routing, and the exact claim
credential used for the attempt, observable in telemetry.
