use serde::{Deserialize, Serialize};

/// Wire protocol spoken by a proxy node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelProtocol {
    #[default]
    Cnexus,
    Aether,
}
