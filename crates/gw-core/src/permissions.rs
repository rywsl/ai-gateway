use crate::ids::*;
use std::collections::HashSet;
use std::net::IpAddr;

#[derive(Clone, Debug)]
pub struct MergedPermissions {
    pub user_id: UserId,
    pub effective_user_id: UserId,
    pub client_ip: IpAddr,
    pub allowed_models: HashSet<ModelName>,
    pub allowed_channels: HashSet<KeyId>,
    pub allowed_groups: HashSet<Group>,
    pub forced_model_group: Option<Group>,
    pub rpm_limit: u32,
    pub tpm_limit: u32,
    pub max_concurrent_streams: u32,
    pub markup_product: f32,
}
