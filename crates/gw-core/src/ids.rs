use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Clone, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

id_newtype!(UserId);
id_newtype!(UserGroupId);
id_newtype!(ApiKeyId);
id_newtype!(VendorId);
id_newtype!(KeyId);
id_newtype!(ModelName);
id_newtype!(Group);
id_newtype!(Kid);
id_newtype!(TraceId);
id_newtype!(RequestId);
