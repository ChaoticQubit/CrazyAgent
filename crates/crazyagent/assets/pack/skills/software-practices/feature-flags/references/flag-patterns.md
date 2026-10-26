# Feature Flag Patterns & Rollout Runbook

## Minimal flag check (pseudocode, applies to any language)

The config is fetched at runtime from a config service, NOT hardcoded. `new_checkout` is the new path and lands disabled first; `legacy_checkout` is the old path and the default.

```
if config.enabled("new_checkout_flow", user=ctx.user):
    return new_checkout(ctx)
else:
    return legacy_checkout(ctx)
```

Key properties:
- Default is the **old/off** behavior.
- The flag is read at runtime so ops can change it without a redeploy.
- One decision point, clearly named; both paths coexist and write compatible data.

## Percentage / staged rollout

The config service returns true for a deterministic hash of the user id, so a given user stays in a stable bucket as the percentage ramps.

```
enabled = config.percentage("new_checkout_flow") >= hash_bucket(user.id)
```

Ramp schedule with metric gates (abort and set back to previous step if a gate fails):

| Step | % of users | Watch |
|------|-----------|-------|
| 1 | 1%  (canary) | error rate, latency p99, crash rate |
| 2 | 5%  | + business KPI (conversion, revenue) |
| 3 | 25% | same |
| 4 | 50% | same |
| 5 | 100% | same, then bake for N days |

## Dark launch

The new pipeline runs for real and exercises load and shadow writes; its metrics are compared against legacy, but the user still sees the old output.

```
result = new_pipeline(ctx)
log_shadow_metrics(result)
return legacy_pipeline(ctx)
```

Once shadow metrics match/beat legacy, switch the return to the new path via a flag ramp.

## Kill switch

Every risky subsystem gets an independent flag defaulting to on-but-abortable. Turning it off degrades gracefully, with no deploy needed:

```
if not config.enabled("recommendations_v2"):
    return []
```

## Rollback via config, not rebuild

Incident response order:
1. Set the flag back to the last-known-good value in the config service (seconds).
2. Confirm metrics recover.
3. Diagnose and fix at trunk; re-ramp later.

Never make an emergency binary push if a config flip can mitigate.

## Cleanup runbook (kill flag debt)

1. Confirm the feature is at 100% and stable for the agreed bake period.
2. Delete the **old** code path.
3. Delete the flag check and the flag definition.
4. Delete now-dead tests for the old path; keep tests for the new (now default) behavior.
5. Close the cleanup ticket filed when the flag was created.

## Tooling notes (language-agnostic)

- Use a real config/experiment service (e.g., OpenFeature-compatible providers, LaunchDarkly, Unleash, Flagsmith, or an internal config service) rather than environment variables baked at deploy time, so flips don't require a redeploy.
- Keep static config in version control and promote it with the release candidate; review config changes like code.
- Emit a metric labeled by flag branch so canary analysis and A/B diffs are possible.
