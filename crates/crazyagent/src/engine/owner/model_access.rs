use crate::protocol::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccessCollection {
    Accounts,
    Connections,
    Pools,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelAccessRecord {
    Account {
        account_ref: CredentialRef,
    },
    Connection {
        connection: crate::config::ModelConnection,
    },
    Pool {
        pool: crate::config::AccountPool,
    },
}

impl ModelAccessRecord {
    pub fn id(&self) -> &CredentialRef {
        match self {
            Self::Account { account_ref } => account_ref,
            Self::Connection { connection } => &connection.id,
            Self::Pool { pool } => &pool.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAccessCursor {
    pub db_instance: String,
    pub owner_id: String,
    pub engine_epoch: String,
    pub owner_epoch: i64,
    pub config_id: ConfigId,
    pub at_revision: Cursor,
    pub source_hash: String,
    pub collection: ModelAccessCollection,
    pub after_id: CredentialRef,
}
