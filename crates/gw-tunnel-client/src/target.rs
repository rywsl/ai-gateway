#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamTarget {
    Tcp { host: String, port: u16, tls: bool },
}

impl StreamTarget {
    pub fn tcp_authority(&self) -> Option<String> {
        match self {
            Self::Tcp { host, port, .. } => Some(format!("{host}:{port}")),
        }
    }

    pub fn tcp_tls_hint(&self) -> Option<bool> {
        match self {
            Self::Tcp { tls, .. } => Some(*tls),
        }
    }
}
