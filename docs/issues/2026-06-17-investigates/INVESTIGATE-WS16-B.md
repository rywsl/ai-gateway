# INVESTIGATE-WS16-B: R2 Warn-Only Requirement

Status: RESOLVED_LOCAL_POLICY

Local finding: `config/default.toml` keeps `signature.replay_check = "none"` as the default. The fix branch adds replay-check modes, but enabling nonce enforcement is a separate operator rollout.

Policy decision: R2 does not need a warn-only window for the default landing release because the default remains no-new-behavior. A warn-only or staged rollout is required before production changes `signature.replay_check` from `none` to `nonce_local` or `nonce_redis`.

Required release note: the R2 advisory must tell operators that nonce enforcement is not active until they set the replay-check mode and ensure clients send non-empty nonce headers.
