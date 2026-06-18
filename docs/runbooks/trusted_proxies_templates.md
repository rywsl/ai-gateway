# Trusted Proxies Templates

`[security].trust_xff = true` only trusts `X-Forwarded-For` when the socket peer
matches `[runtime].trusted_proxies`. Keep the default empty list unless the
gateway is behind load balancers or sidecars that strip client-supplied XFF.

Production rejects `trust_xff = true` with an empty list using
`production_trusted_proxies_empty`. Production also rejects wildcard CIDRs
(`0.0.0.0/0`, `::/0`) using `production_trusted_proxies_wildcard`.

## Private Cluster Template

```toml
[security]
trust_xff = true

[runtime]
trusted_proxies = [
  "10.0.0.0/8",
  "172.16.0.0/12",
  "192.168.0.0/16",
]
```

## AWS Load Balancer Template

Prefer the exact VPC subnet CIDRs used by the ALB/NLB or ingress controller:

```toml
[security]
trust_xff = true

[runtime]
trusted_proxies = [
  "10.10.0.0/20",
  "10.10.16.0/20",
]
```

## GCP Load Balancer Template

Prefer the exact proxy-only subnet or GKE ingress/NEG proxy CIDRs:

```toml
[security]
trust_xff = true

[runtime]
trusted_proxies = [
  "10.20.0.0/23",
]
```
