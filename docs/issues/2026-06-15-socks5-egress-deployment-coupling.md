# SOCKS5 Egress Deployment Coupling

Status: documented operational gotcha

Affected: `gw-tunnel-server` Cnexus tunnel nodes

## Summary

`proxy_nodes.proxy_url` is an operator-facing record. It does not push runtime egress config into a running `gw-tunnel-server`.

The actual Cnexus SOCKS5 egress setting is read by the tunnel-server process from `--proxy-url` or, if absent, `PROXY_URL`.

## Failure Mode

If the database row says:

```text
proxy_url = socks5h://prod-proxy:1080
```

but the deployed server was started with:

```text
--proxy-url socks5h://staging-proxy:1080
```

traffic exits through `staging-proxy`. The database value is not authoritative for the running process.

## Recommended Pattern

Use one deployment source of truth for both operations:

- write or display the node `proxy_url` value in the admin plane
- redeploy or restart `gw-tunnel-server` with the same value as `PROXY_URL`

This preserves the Aether-era migration contract: an existing `proxy_url = socks5://...` can be lifted into the Cnexus tunnel-server deployment without changing upstream egress behavior.

## Supported Schemes

- `socks5`: local DNS resolution
- `socks5h`: DNS resolution at the proxy

HTTP CONNECT proxying remains out of scope for this phase.
