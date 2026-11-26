pub mod ports;
pub mod service;

use chrono::{DateTime, Utc};
use matos_common::OrganizationId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub organization_id: OrganizationId,
    pub subject: String,
    pub name: String,
    pub email: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}
