# Billing Model Profiles

## Overview

NEEDLE supports three billing model behavior profiles that adjust strand execution and priority handling to optimize cost vs throughput based on your billing structure.

## Billing Models

### pay_per_token (Default)

**Use when:** You pay per API call and want to minimize costs.

**Behavior:**
- **Budget enforcement:** Strict (stops at 100% of daily budget)
- **Priority threshold:** Conservative (only P0-P1 critical/high priority beads)
- **Strand enablement:** Essential strands only (pluck, explore, mend, knot)
- **Worker concurrency:** Low (3 workers default)
- **Priority weights:** Reduced for lower priorities (P2+ get 50% weight reduction)

**Best for:** Cost-conscious workflows, pay-as-you-go plans, development/testing

### use_or_lose

**Use when:** You have a fixed daily budget allocation that doesn't roll over.

**Behavior:**
- **Budget enforcement:** Target (allows up to 120% overrun, budget is goal not limit)
- **Priority threshold:** Moderate (P0-P2 critical/high/normal priority beads)
- **Strand enablement:** All strands enabled including opt-in (weave, unravel, pulse)
- **Worker concurrency:** High (8 workers default)
- **Priority weights:** Boosted for higher priorities (P0-P2 get 50% weight boost)

**Best for:** Fixed-budget plans, "use it or lose it" allocations, maximizing daily value

### unlimited

**Use when:** You have unlimited API access or very high limits.

**Behavior:**
- **Budget enforcement:** None (no budget limits enforced)
- **Priority threshold:** Maximum (all priorities P0-P4+ processed)
- **Strand enablement:** All strands enabled except weave (still opt-in)
- **Worker concurrency:** Maximum (20 workers default)
- **Priority weights:** Base weights (no adjustments)

**Best for:** Enterprise plans, unlimited API keys, local inference, maximum throughput needed

## Configuration

### Basic Configuration

```yaml
# ~/.needle/config.yaml

billing:
  # model: Billing model profile
  #   - pay_per_token: Conservative (default), minimize token usage
  #   - use_or_lose: Aggressive, use allocated budget
  #   - unlimited: Maximum throughput, no budget enforcement
  model: pay_per_token

  # daily_budget_usd: Daily budget in USD
  daily_budget_usd: 10.0

strands:
  # Values: true (always enabled), false (always disabled), auto (follows billing model)
  pluck: auto    # Primary work from configured workspaces
  explore: auto  # Look for work in other workspaces
  mend: auto     # Maintenance and cleanup
  weave: auto    # Create beads from documentation gaps
  unravel: auto  # Create alternatives for blocked beads
  pulse: auto    # Codebase health monitoring
  knot: auto     # Alert human when stuck
```

### Example: Pay Per Token (Conservative)

```yaml
billing:
  model: pay_per_token
  daily_budget_usd: 5.0

strands:
  pluck: auto      # ✓ Enabled (essential)
  explore: auto    # ✓ Enabled (essential)
  mend: auto       # ✓ Enabled (essential)
  weave: auto      # ✗ Disabled (opt-in)
  unravel: auto    # ✗ Disabled (opt-in)
  pulse: auto      # ✗ Disabled (opt-in)
  knot: auto       # ✓ Enabled (essential)
```

**Result:**
- Processes only P0-P1 (critical/high) priority beads
- Stops at $5.00 daily spend (100% of budget)
- Runs 3 concurrent workers
- Only essential strands active

### Example: Use or Lose (Aggressive)

```yaml
billing:
  model: use_or_lose
  daily_budget_usd: 50.0

strands:
  pluck: auto      # ✓ Enabled
  explore: auto    # ✓ Enabled
  mend: auto       # ✓ Enabled
  weave: auto      # ✓ Enabled (opt-in now active)
  unravel: auto    # ✓ Enabled (opt-in now active)
  pulse: auto      # ✓ Enabled (opt-in now active)
  knot: auto       # ✓ Enabled
```

**Result:**
- Processes P0-P2 (critical/high/normal) priority beads
- Allows overrun up to $60.00 (120% of budget)
- Runs 8 concurrent workers
- All strands active including opt-in

### Example: Unlimited (Maximum)

```yaml
billing:
  model: unlimited

strands:
  pluck: auto      # ✓ Enabled
  explore: auto    # ✓ Enabled
  mend: auto       # ✓ Enabled
  weave: auto      # ✗ Disabled (opt-in only)
  unravel: auto    # ✓ Enabled
  pulse: auto      # ✓ Enabled
  knot: auto       # ✓ Enabled
```

**Result:**
- Processes all priorities (P0-P4+)
- No budget enforcement
- Runs 20 concurrent workers
- All strands active except weave (still opt-in)

### Explicit Strand Override

You can override billing model defaults for individual strands:

```yaml
billing:
  model: pay_per_token  # Conservative
  daily_budget_usd: 10.0

strands:
  pluck: auto    # ✓ Enabled (follows billing model)
  explore: auto  # ✓ Enabled (follows billing model)
  mend: auto     # ✓ Enabled (follows billing model)
  weave: true    # ✓ Enabled (explicit override)
  unravel: false # ✗ Disabled (explicit override)
  pulse: auto    # ✗ Disabled (follows billing model)
  knot: auto     # ✓ Enabled (follows billing model)
```

## Behavior Details

### Budget Enforcement Strategy

| Model         | Strategy | Stop Threshold | Behavior                          |
|---------------|----------|----------------|-----------------------------------|
| pay_per_token | strict   | 100%           | Hard stop at budget limit         |
| use_or_lose   | target   | 120%           | Budget is goal, allows overrun    |
| unlimited     | none     | Never          | No budget enforcement             |

### Priority Thresholds

| Model         | Min Priority | Processes    |
|---------------|--------------|--------------|
| pay_per_token | P1           | P0-P1 only   |
| use_or_lose   | P2           | P0-P2        |
| unlimited     | P4           | All (P0-P4+) |

### Priority Weight Adjustments

| Priority | Base Weight | pay_per_token | use_or_lose | unlimited |
|----------|-------------|---------------|-------------|-----------|
| P0       | 8x          | 8x            | 12x (+50%)  | 8x        |
| P1       | 4x          | 4x            | 6x (+50%)   | 4x        |
| P2       | 2x          | 1x (-50%)     | 3x (+50%)   | 2x        |
| P3       | 1x          | 0.5x (-50%)   | 1x          | 1x        |
| P4+      | 1x          | 0.5x (-50%)   | 1x          | 1x        |

### Default Concurrency

| Model         | Default Workers | Override Example                     |
|---------------|-----------------|--------------------------------------|
| pay_per_token | 3               | `limits.global_max_concurrent: 5`    |
| use_or_lose   | 8               | `limits.global_max_concurrent: 10`   |
| unlimited     | 20              | `limits.global_max_concurrent: 50`   |

## Command-Line Testing

You can test billing model behavior directly:

```bash
# Get current billing model
./src/lib/billing_models.sh model

# Get enforcement strategy
./src/lib/billing_models.sh strategy pay_per_token

# Check if strand is enabled
./src/lib/billing_models.sh strand-enabled weave pay_per_token

# Check if should stop for budget
./src/lib/billing_models.sh should-stop 55 50 pay_per_token

# Display full profile
./src/lib/billing_models.sh show
```

## Testing

Comprehensive tests are available:

```bash
./tests/test_billing_models.sh
```

Tests cover:
- Billing model selection (default, pay_per_token, use_or_lose, unlimited)
- Budget enforcement strategies (strict, target, none)
- Priority thresholds (P0-P1, P0-P2, P0-P4+)
- Worker concurrency defaults
- Strand enablement (auto, explicit true/false)
- Budget stop logic (100%, 120%, never)
- Priority weight adjustments

## Migration Guide

### From Explicit Configuration

**Before:**
```yaml
strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: false
  pulse: false
  knot: true

effort:
  budget:
    daily_limit_usd: 50.0
```

**After (pay_per_token):**
```yaml
billing:
  model: pay_per_token
  daily_budget_usd: 10.0

strands:
  pluck: auto
  explore: auto
  mend: auto
  weave: auto
  unravel: auto
  pulse: auto
  knot: auto
```

### From Legacy Budget Config

The `effort.budget.daily_limit_usd` is still supported for backward compatibility, but `billing.daily_budget_usd` takes precedence.

**Legacy (still works):**
```yaml
effort:
  budget:
    daily_limit_usd: 50.0
```

**Recommended:**
```yaml
billing:
  model: pay_per_token
  daily_budget_usd: 50.0
```

## Best Practices

1. **Start conservative:** Begin with `pay_per_token` and monitor costs
2. **Use auto strands:** Let billing model control strand enablement
3. **Set realistic budgets:** Budget should match your API plan limits
4. **Monitor usage:** Check `needle status` regularly for budget status
5. **Adjust as needed:** Switch models based on workload and costs

## Troubleshooting

### Budget exceeded too quickly

```yaml
# Increase budget or switch to less aggressive model
billing:
  model: pay_per_token  # More conservative
  daily_budget_usd: 20.0  # Increase budget
```

### Not enough work getting done

```yaml
# Switch to more aggressive model or lower thresholds
billing:
  model: use_or_lose  # More aggressive
  daily_budget_usd: 50.0
```

### Specific strand not running

```yaml
# Override billing model for specific strand
strands:
  pulse: true  # Force enable pulse strand
```

## Implementation Details

See:
- `src/lib/billing_models.sh` - Core implementation
- `src/lib/config.sh` - Configuration defaults
- `src/telemetry/budget.sh` - Budget enforcement
- `src/strands/engine.sh` - Strand enablement
- `src/bead/select.sh` - Priority weights
- `tests/test_billing_models.sh` - Test suite
