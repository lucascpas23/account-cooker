use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use account_cooker_core::{AccountMetaSpec, InstructionSpec, PlannedAction};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::{ExecutionTransport, SchedulerError, SubmissionOutcome};

/// Minimal JSON-RPC loopback transport. It cannot target a non-loopback host and always
/// simulates the exact signed transaction before `sendTransaction`.
pub struct LoopbackRpcTransport {
    rpc_url: Url,
    signer: Arc<Keypair>,
    client: reqwest::Client,
    simulated_transactions: tokio::sync::Mutex<BTreeMap<uuid::Uuid, Vec<u8>>>,
}

impl LoopbackRpcTransport {
    pub fn new(rpc_url: &str, signer: Arc<Keypair>) -> Result<Self, SchedulerError> {
        let rpc_url = Url::parse(rpc_url)
            .map_err(|error| SchedulerError::Transport(format!("invalid RPC URL: {error}")))?;
        let is_loopback = rpc_url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !is_loopback {
            return Err(SchedulerError::Transport(
                "LoopbackRpcTransport refuses non-loopback RPC URLs".into(),
            ));
        }
        Ok(Self {
            rpc_url,
            signer,
            client: reqwest::Client::new(),
            simulated_transactions: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub fn signer_public_key(&self) -> String {
        self.signer.pubkey().to_string()
    }

    pub async fn validate_identity(&self) -> Result<String, SchedulerError> {
        let _: String = self.call("getHealth", json!([])).await?;
        let genesis_hash: String = self.call("getGenesisHash", json!([])).await?;
        let version: Value = self.call("getVersion", json!([])).await?;
        if !version.is_object() {
            return Err(SchedulerError::Transport(
                "loopback RPC getVersion did not identify a Solana validator".into(),
            ));
        }
        Ok(genesis_hash)
    }

    pub async fn balance(&self) -> Result<u64, SchedulerError> {
        let result: Value = self
            .call(
                "getBalance",
                json!([self.signer_public_key(), {"commitment":"confirmed"}]),
            )
            .await?;
        result
            .get("value")
            .and_then(Value::as_u64)
            .ok_or_else(|| SchedulerError::Transport("getBalance omitted value".into()))
    }

    /// Funds the ephemeral signer only on the configured loopback validator and waits for
    /// signature status. This is intended for isolated Surfpool acceptance runs.
    pub async fn request_local_airdrop(&self, lamports: u64) -> Result<String, SchedulerError> {
        self.validate_identity().await?;
        let signature: String = self
            .call(
                "requestAirdrop",
                json!([self.signer_public_key(), lamports, {"commitment":"confirmed"}]),
            )
            .await?;
        for _ in 0..40 {
            if self.reconcile(&signature).await? == Some(true) {
                return Ok(signature);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Err(SchedulerError::Transport(
            "loopback airdrop was not confirmed within the bounded wait".into(),
        ))
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, SchedulerError> {
        let response = self
            .client
            .post(self.rpc_url.clone())
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .map_err(|error| {
                SchedulerError::Transport(format!("loopback RPC request failed: {error}"))
            })?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|error| {
            SchedulerError::Transport(format!("invalid loopback RPC response: {error}"))
        })?;
        if !status.is_success() || body.get("error").is_some() {
            return Err(SchedulerError::Transport(format!(
                "loopback RPC {method} rejected request: {}",
                body.get("error").unwrap_or(&body)
            )));
        }
        serde_json::from_value(body.get("result").cloned().unwrap_or(Value::Null)).map_err(
            |error| {
                SchedulerError::Transport(format!(
                    "unexpected loopback RPC {method} result: {error}"
                ))
            },
        )
    }

    async fn signed_transaction(
        &self,
        specs: &[InstructionSpec],
    ) -> Result<Transaction, SchedulerError> {
        let blockhash_result: Value = self
            .call("getLatestBlockhash", json!([{"commitment":"confirmed"}]))
            .await?;
        let blockhash = blockhash_result
            .get("value")
            .and_then(|value| value.get("blockhash"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SchedulerError::Transport("getLatestBlockhash omitted blockhash".into())
            })?;
        let blockhash = Hash::from_str(blockhash).map_err(|error| {
            SchedulerError::Transport(format!("invalid loopback blockhash: {error}"))
        })?;
        let instructions: Vec<Instruction> = specs
            .iter()
            .map(spec_to_instruction)
            .collect::<Result<_, _>>()?;
        let signer_key = self.signer.pubkey();
        for required in instructions
            .iter()
            .flat_map(|ix| ix.accounts.iter())
            .filter(|meta| meta.is_signer)
        {
            if required.pubkey != signer_key {
                return Err(SchedulerError::Transport(format!(
                    "ephemeral signer {} cannot authorize required signer {}",
                    signer_key, required.pubkey
                )));
            }
        }
        let message = Message::new(&instructions, Some(&signer_key));
        let mut transaction = Transaction::new_unsigned(message);
        transaction
            .try_sign(&[self.signer.as_ref()], blockhash)
            .map_err(|error| {
                SchedulerError::Transport(format!("transaction signing failed: {error}"))
            })?;
        Ok(transaction)
    }
}

#[async_trait]
impl ExecutionTransport for LoopbackRpcTransport {
    async fn simulate(
        &self,
        action: &PlannedAction,
        instructions: &[InstructionSpec],
    ) -> Result<(), SchedulerError> {
        let transaction = self.signed_transaction(instructions).await?;
        let bytes = bincode::serialize(&transaction).map_err(|error| {
            SchedulerError::Transport(format!("transaction serialization failed: {error}"))
        })?;
        let encoded = BASE64.encode(&bytes);
        let result: Value = self
            .call(
                "simulateTransaction",
                json!([encoded,{"encoding":"base64","sigVerify":true,"commitment":"confirmed","replaceRecentBlockhash":false}]),
            )
            .await?;
        if let Some(error) = result.get("value").and_then(|value| value.get("err"))
            && !error.is_null()
        {
            return Err(SchedulerError::Transport(format!(
                "loopback simulation failed: {error}"
            )));
        }
        self.simulated_transactions
            .lock()
            .await
            .insert(action.id, bytes);
        Ok(())
    }

    async fn submit(
        &self,
        action: &PlannedAction,
        _: &[InstructionSpec],
    ) -> Result<SubmissionOutcome, SchedulerError> {
        let bytes = self
            .simulated_transactions
            .lock()
            .await
            .remove(&action.id)
            .ok_or_else(|| {
                SchedulerError::Transport(
                    "refusing submission without cached exact simulated transaction".into(),
                )
            })?;
        let transaction: Transaction = bincode::deserialize(&bytes).map_err(|error| {
            SchedulerError::Transport(format!("cached transaction is invalid: {error}"))
        })?;
        let local_signature = transaction
            .signatures
            .first()
            .ok_or_else(|| SchedulerError::Transport("signed transaction has no signature".into()))?
            .to_string();
        let encoded = BASE64.encode(&bytes);
        let rpc_signature: Result<String, SchedulerError> = self
            .call(
                "sendTransaction",
                json!([encoded,{"encoding":"base64","skipPreflight":false,"preflightCommitment":"confirmed","maxRetries":0}]),
            ).await;
        let signature = match rpc_signature {
            Ok(signature) if signature == local_signature => signature,
            Ok(signature) => {
                return Err(SchedulerError::Transport(format!(
                    "RPC returned signature {signature}, expected {local_signature}"
                )));
            }
            Err(_) => {
                return Ok(SubmissionOutcome::Unknown {
                    signature: Some(local_signature),
                });
            }
        };
        for _ in 0..30 {
            if self.reconcile(&signature).await? == Some(true) {
                return Ok(SubmissionOutcome::Confirmed { signature });
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok(SubmissionOutcome::Unknown {
            signature: Some(signature),
        })
    }

    async fn reconcile(&self, signature: &str) -> Result<Option<bool>, SchedulerError> {
        let status: Value = self
            .call(
                "getSignatureStatuses",
                json!([[signature],{"searchTransactionHistory":true}]),
            )
            .await?;
        Ok(status
            .get("value")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| {
                if row.is_null() {
                    None
                } else {
                    Some(row.get("err").is_some_and(Value::is_null))
                }
            }))
    }
}

fn spec_to_instruction(spec: &InstructionSpec) -> Result<Instruction, SchedulerError> {
    Ok(Instruction {
        program_id: Pubkey::from_str(&spec.program_id)
            .map_err(|_| SchedulerError::Transport("adapter emitted invalid program ID".into()))?,
        accounts: spec
            .accounts
            .iter()
            .map(account_meta)
            .collect::<Result<_, _>>()?,
        data: spec.data.clone(),
    })
}

fn account_meta(spec: &AccountMetaSpec) -> Result<AccountMeta, SchedulerError> {
    let key = Pubkey::from_str(&spec.public_key)
        .map_err(|_| SchedulerError::Transport("adapter emitted invalid account key".into()))?;
    Ok(match (spec.writable, spec.signer) {
        (true, signer) => AccountMeta::new(key, signer),
        (false, signer) => AccountMeta::new_readonly(key, signer),
    })
}

#[cfg(test)]
mod tests {
    use account_cooker_core::{ActionKind, ExecutionState, PlannerModel};
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn transport_rejects_public_rpc_at_construction() {
        assert!(
            LoopbackRpcTransport::new(
                "https://api.mainnet-beta.solana.com",
                Arc::new(Keypair::new())
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn transport_refuses_send_without_exact_simulation_cache() {
        let transport =
            LoopbackRpcTransport::new("http://127.0.0.1:9", Arc::new(Keypair::new())).unwrap();
        let action = PlannedAction {
            id: Uuid::new_v4(),
            fleet_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            scheduled_at: Utc::now(),
            kind: ActionKind::Memo,
            adapter_id: "memo".into(),
            amount_lamports: 0,
            counterparty: None,
            asset: "SOL".into(),
            state: ExecutionState::Submitted,
            idempotency_key: "never-send-without-simulation".into(),
            model: PlannerModel::PersonaSession,
            seed_tag: "test".into(),
            session_id: None,
        };
        let error = transport.submit(&action, &[]).await.unwrap_err();
        assert!(error.to_string().contains("exact simulated transaction"));
    }
}
