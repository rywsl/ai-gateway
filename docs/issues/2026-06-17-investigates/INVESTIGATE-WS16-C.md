# INVESTIGATE-WS16-C: R3 Static PEM Configuration Source

Status: RESOLVED_SOURCE_CHECK

Local finding: the Rust source defines `JwtValidatorConfig.static_pem_rs256` and `allow_static_pem_fallback_for_unknown_kid` in `crates/gw-auth/src/jwt.rs`; gateway wiring passes those values through `crates/gw-gateway/src/lib.rs`. The local source search did not find a second Go-side auth-config table that would expand the R3 query scope.

Impact on audit query: `internal-audit/queries.sql` intentionally targets the warehouse rollup field pair `has_static_pem_rs256` and `has_jwks_url`, rather than naming a product table that is not present in this checkout.

Residual external dependency: the warehouse owner must confirm that `security_audit_jwt_config_rollup` is populated from the same runtime config source before replacing the schema-only CSV.
