-- WS-16 affected-customer audit templates.
-- Run against a production read-replica or warehouse snapshot only.
-- Do not select customer names, email addresses, tokens, raw JWTs, or payloads.

-- R1: tenants that may fall back to legacy JWT validation.
-- Replace table names if the warehouse schema differs.
SELECT
  tenant_id,
  MIN(first_seen_jwt_ts) AS first_seen_jwt_ts,
  MAX(last_seen_jwt_ts) AS last_seen_jwt_ts,
  SUM(request_count_7d) AS request_count_7d
FROM security_audit_jwt_tenant_rollup
WHERE has_auth_config_global = false
GROUP BY tenant_id;

-- R2: tenants with signed requests where nonce replay enforcement is not active.
SELECT
  tenant_id,
  MIN(signature_required_since) AS signature_required_since,
  SUM(request_count_7d) AS request_count_7d,
  SUM(distinct_ip_count_7d) AS distinct_ip_count_7d
FROM security_audit_signature_rollup
WHERE signature_required = true
  AND replay_check_mode IN ('none', '')
GROUP BY tenant_id;

-- R3: tenants that combine static PEM and JWKS.
SELECT
  tenant_id,
  MAX(static_pem_age_days) AS static_pem_age_days,
  MAX(jwks_url_hash) AS jwks_url_hash,
  MAX(last_kid_seen) AS last_kid_seen
FROM security_audit_jwt_config_rollup
WHERE has_static_pem_rs256 = true
  AND has_jwks_url = true
GROUP BY tenant_id;

-- R4: deployments that trust XFF without a trusted boundary.
SELECT
  tenant_id,
  deploy_env,
  public_exposure,
  trust_xff,
  trusted_proxy_count,
  CASE
    WHEN public_exposure = true AND trust_xff = true THEN 'p0_if_public_internet'
    WHEN trust_xff = true THEN 'p1_lb_only'
    ELSE 'not_affected'
  END AS severity_for_this_tenant
FROM security_audit_runtime_config_rollup
WHERE trust_xff = true;
