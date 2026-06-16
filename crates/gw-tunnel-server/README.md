# gw-tunnel-server

First-party server for the `ailinkmax-tunnel` WebSocket protocol.

## Egress Proxy

`gw-tunnel-server` can dial upstream targets through a SOCKS5 proxy:

```bash
gw-tunnel-server --proxy-url socks5h://proxy.example.com:1080
PROXY_URL=socks5://user:pass@proxy.example.com:1080 gw-tunnel-server
```

Supported schemes:

- `socks5`: resolve target DNS locally, then send the resolved IP to the proxy.
- `socks5h`: send the hostname to the proxy and let the proxy resolve DNS.

`--proxy-url` takes precedence over `PROXY_URL`. Missing or empty config keeps direct egress. Userinfo is accepted in standard URL form and percent-decoded before SOCKS5 auth.

For a first-party Cnexus WebSocket tunnel-server node, the server process reads only its deployment config, so deployers should still inject the desired egress proxy into `--proxy-url` or `PROXY_URL`.

Separately, the gateway can also route selected proxy-node rows directly through `proxy_nodes.payload.proxy_url` when that value is `socks5://...` or `socks5h://...`. That compatibility path is for old SOCKS5 proxy links that should remain usable without deploying `gw-tunnel-server`.

See `docs/spec/09-tunnel.md` for the full tunnel contract.
