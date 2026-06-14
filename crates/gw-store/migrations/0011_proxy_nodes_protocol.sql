-- 0011_proxy_nodes_protocol.sql - add `tunnel_protocol` discriminator to
-- proxy_nodes payload (cnexus default; aether variant lands in Phase 2 of the
-- tunnel-compat design at docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md).
--
-- Schema shape is unchanged: entity table rows still carry payload/version
-- columns. Only the JSONB payload schema is extended.

UPDATE proxy_nodes
SET payload = jsonb_set(payload, '{tunnel_protocol}', '"cnexus"', true)
WHERE payload->>'tunnel_protocol' IS NULL;

CREATE INDEX IF NOT EXISTS proxy_nodes_tunnel_protocol_idx
  ON proxy_nodes ((payload->>'tunnel_protocol'))
  WHERE deleted_at IS NULL;
