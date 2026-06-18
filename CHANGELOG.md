# Changelog

## Unreleased

### Security

- AILMX-2026-001: Legacy JWT validation accepted incomplete tenant claims.
  - Severity: P0.
  - Fix branch evidence: `ailinkmax-gateway` commit `4afff41fefbf7876d69af2dedb2406fbfad2912a`; mirrored Go/security artifacts in commit `78845c7a49e8869fb0291e18bd356466400f0547`.
  - Release version, production rollout, and publication remain blocked until the WS-16 launch gate is satisfied.
- AILMX-2026-002: HMAC request signatures lacked replay nonce enforcement.
  - Severity: P0.
  - Fix branch evidence: `ailinkmax-gateway` commit `4afff41fefbf7876d69af2dedb2406fbfad2912a`; mirrored Go/security artifacts in commit `78845c7a49e8869fb0291e18bd356466400f0547`.
  - Release version, production rollout, and publication remain blocked until the WS-16 launch gate is satisfied.
- AILMX-2026-003: JWT key-id fallback could widen static key exposure.
  - Severity: P0.
  - Fix branch evidence: `ailinkmax-gateway` commit `4afff41fefbf7876d69af2dedb2406fbfad2912a`; mirrored Go/security artifacts in commit `78845c7a49e8869fb0291e18bd356466400f0547`.
  - Release version, production rollout, and publication remain blocked until the WS-16 launch gate is satisfied.
- AILMX-2026-004: Trusting client X-Forwarded-For can weaken rate-limit identity.
  - Severity: P1 by default configuration; can become P0 for public-internet deployments with `trust_xff = true`.
  - Fix branch evidence: `ailinkmax-gateway` commit `4afff41fefbf7876d69af2dedb2406fbfad2912a`; mirrored Go/security artifacts in commit `78845c7a49e8869fb0291e18bd356466400f0547`.
  - Release version, production rollout, and publication remain blocked until the WS-16 launch gate is satisfied.
