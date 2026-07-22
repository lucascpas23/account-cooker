use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ActionKind, PlannedAction};

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("invalid adapter configuration: {0}")]
    InvalidConfiguration(String),
    #[error("unsupported action {0:?}")]
    UnsupportedAction(ActionKind),
    #[error("insufficient balance: required {required}, available {available}")]
    InsufficientBalance { required: u64, available: u64 },
    #[error("transaction construction failed: {0}")]
    Construction(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountMetaSpec {
    pub public_key: String,
    pub signer: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstructionSpec {
    pub program_id: String,
    pub accounts: Vec<AccountMetaSpec>,
    pub data: Vec<u8>,
    pub redacted_description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedChange {
    pub account: String,
    pub lamports_delta: i64,
    pub token_delta: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterContext {
    pub signer_public_key: String,
    pub counterparty_public_key: Option<String>,
    pub mint_public_key: Option<String>,
    pub available_lamports: u64,
    pub available_tokens: u64,
}

pub trait ProtocolAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn validate_configuration(&self, config: &BTreeMap<String, String>)
    -> Result<(), AdapterError>;
    fn supported_actions(&self) -> &'static [ActionKind];
    fn required_program_ids(&self) -> Vec<String>;
    fn estimate_lamports(&self, action: &PlannedAction) -> Result<u64, AdapterError>;
    fn build_instructions(
        &self,
        action: &PlannedAction,
        context: &AdapterContext,
    ) -> Result<Vec<InstructionSpec>, AdapterError>;
    fn expected_changes(
        &self,
        action: &PlannedAction,
        context: &AdapterContext,
    ) -> Result<Vec<ExpectedChange>, AdapterError>;
    fn safety_classification(&self) -> &'static str;
}
