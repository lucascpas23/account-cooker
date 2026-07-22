#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::str::FromStr;

use account_cooker_config::{
    ATA_PROGRAM_ID, MEMO_PROGRAM_ID, STAKE_PROGRAM_ID, SYSTEM_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use account_cooker_core::{
    AccountMetaSpec, ActionKind, AdapterContext, AdapterError, ExpectedChange, InstructionSpec,
    PlannedAction, ProtocolAdapter,
};
use sha2::{Digest, Sha256};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

#[derive(Debug, Default)]
pub struct NativeSolAdapter;

impl ProtocolAdapter for NativeSolAdapter {
    fn id(&self) -> &'static str {
        "native-sol"
    }

    fn validate_configuration(&self, _: &BTreeMap<String, String>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn supported_actions(&self) -> &'static [ActionKind] {
        &[ActionKind::NativeTransfer, ActionKind::Consolidate]
    }

    fn required_program_ids(&self) -> Vec<String> {
        vec![SYSTEM_PROGRAM_ID.into()]
    }

    fn estimate_lamports(&self, action: &PlannedAction) -> Result<u64, AdapterError> {
        Ok(action.amount_lamports.saturating_add(5_000))
    }

    fn build_instructions(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<InstructionSpec>, AdapterError> {
        require_balance(ctx, self.estimate_lamports(action)?)?;
        let from = parse_key(&ctx.signer_public_key)?;
        let to = parse_counterparty(ctx)?;
        Ok(vec![instruction_spec(
            solana_system_interface::instruction::transfer(&from, &to, action.amount_lamports),
            "bounded native SOL peer transfer",
        )])
    }

    fn expected_changes(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<ExpectedChange>, AdapterError> {
        Ok(vec![
            ExpectedChange {
                account: ctx.signer_public_key.clone(),
                lamports_delta: -(action.amount_lamports.min(i64::MAX as u64) as i64),
                token_delta: None,
            },
            ExpectedChange {
                account: ctx
                    .counterparty_public_key
                    .clone()
                    .ok_or_else(|| AdapterError::Construction("counterparty required".into()))?,
                lamports_delta: action.amount_lamports.min(i64::MAX as u64) as i64,
                token_delta: None,
            },
        ])
    }

    fn safety_classification(&self) -> &'static str {
        "bounded-value-transfer"
    }
}

#[derive(Debug, Default)]
pub struct MemoAdapter;

impl ProtocolAdapter for MemoAdapter {
    fn id(&self) -> &'static str {
        "memo"
    }

    fn validate_configuration(&self, _: &BTreeMap<String, String>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn supported_actions(&self) -> &'static [ActionKind] {
        &[ActionKind::Memo, ActionKind::Browse]
    }

    fn required_program_ids(&self) -> Vec<String> {
        vec![MEMO_PROGRAM_ID.into()]
    }

    fn estimate_lamports(&self, _: &PlannedAction) -> Result<u64, AdapterError> {
        Ok(5_000)
    }

    fn build_instructions(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<InstructionSpec>, AdapterError> {
        require_balance(ctx, self.estimate_lamports(action)?)?;
        let signer = parse_key(&ctx.signer_public_key)?;
        let program = parse_key(MEMO_PROGRAM_ID)?;
        let digest = Sha256::digest(action.idempotency_key.as_bytes());
        let memo = format!("account-cooker:v1:{}", short_hex(&digest[..8]));
        Ok(vec![instruction_spec(
            spl_memo_interface::instruction::build_memo(&program, memo.as_bytes(), &[&signer]),
            "non-sensitive bounded memo",
        )])
    }

    fn expected_changes(
        &self,
        _: &PlannedAction,
        _: &AdapterContext,
    ) -> Result<Vec<ExpectedChange>, AdapterError> {
        Ok(Vec::new())
    }

    fn safety_classification(&self) -> &'static str {
        "metadata-only"
    }
}

#[derive(Debug, Default)]
pub struct SplTokenAdapter;

impl ProtocolAdapter for SplTokenAdapter {
    fn id(&self) -> &'static str {
        "spl-token"
    }

    fn validate_configuration(
        &self,
        config: &BTreeMap<String, String>,
    ) -> Result<(), AdapterError> {
        if let Some(decimals) = config.get("decimals") {
            decimals
                .parse::<u8>()
                .map_err(|_| AdapterError::InvalidConfiguration("decimals must be u8".into()))?;
        }
        Ok(())
    }

    fn supported_actions(&self) -> &'static [ActionKind] {
        &[ActionKind::SplTokenTransfer]
    }

    fn required_program_ids(&self) -> Vec<String> {
        vec![TOKEN_PROGRAM_ID.into(), ATA_PROGRAM_ID.into()]
    }

    fn estimate_lamports(&self, _: &PlannedAction) -> Result<u64, AdapterError> {
        Ok(2_100_000)
    }

    fn build_instructions(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<InstructionSpec>, AdapterError> {
        require_balance(ctx, self.estimate_lamports(action)?)?;
        if ctx.available_tokens < action.amount_lamports {
            return Err(AdapterError::InsufficientBalance {
                required: action.amount_lamports,
                available: ctx.available_tokens,
            });
        }
        let owner = parse_key(&ctx.signer_public_key)?;
        let recipient = parse_counterparty(ctx)?;
        let mint = parse_key(
            ctx.mint_public_key
                .as_deref()
                .ok_or_else(|| AdapterError::Construction("mint required".into()))?,
        )?;
        let token_program = parse_key(TOKEN_PROGRAM_ID)?;
        let source = spl_associated_token_account_interface::address::get_associated_token_address_with_program_id(&owner, &mint, &token_program);
        let destination = spl_associated_token_account_interface::address::get_associated_token_address_with_program_id(&recipient, &mint, &token_program);
        let create = spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent(&owner, &recipient, &mint, &token_program);
        let transfer = spl_token_interface::instruction::transfer_checked(
            &token_program,
            &source,
            &mint,
            &destination,
            &owner,
            &[],
            action.amount_lamports,
            6,
        )
        .map_err(|e| AdapterError::Construction(e.to_string()))?;
        Ok(vec![
            instruction_spec(create, "idempotent associated token account creation"),
            instruction_spec(transfer, "bounded checked SPL token transfer"),
        ])
    }

    fn expected_changes(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<ExpectedChange>, AdapterError> {
        Ok(vec![
            ExpectedChange {
                account: ctx.signer_public_key.clone(),
                lamports_delta: 0,
                token_delta: Some(-(action.amount_lamports.min(i64::MAX as u64) as i64)),
            },
            ExpectedChange {
                account: ctx
                    .counterparty_public_key
                    .clone()
                    .ok_or_else(|| AdapterError::Construction("counterparty required".into()))?,
                lamports_delta: 0,
                token_delta: Some(action.amount_lamports.min(i64::MAX as u64) as i64),
            },
        ])
    }

    fn safety_classification(&self) -> &'static str {
        "bounded-token-transfer"
    }
}

#[derive(Debug, Default)]
pub struct NativeStakeAdapter;

impl ProtocolAdapter for NativeStakeAdapter {
    fn id(&self) -> &'static str {
        "native-stake"
    }

    fn validate_configuration(
        &self,
        config: &BTreeMap<String, String>,
    ) -> Result<(), AdapterError> {
        if let Some(vote) = config.get("vote_account") {
            parse_key(vote)?;
        }
        Ok(())
    }

    fn supported_actions(&self) -> &'static [ActionKind] {
        &[
            ActionKind::StakeCreate,
            ActionKind::StakeDeactivate,
            ActionKind::StakeWithdraw,
        ]
    }

    fn required_program_ids(&self) -> Vec<String> {
        vec![STAKE_PROGRAM_ID.into(), SYSTEM_PROGRAM_ID.into()]
    }

    fn estimate_lamports(&self, action: &PlannedAction) -> Result<u64, AdapterError> {
        Ok(action.amount_lamports.saturating_add(3_000_000))
    }

    fn build_instructions(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<InstructionSpec>, AdapterError> {
        let authority = parse_key(&ctx.signer_public_key)?;
        let stake_program = parse_key(STAKE_PROGRAM_ID)?;
        let seed = &action.idempotency_key[..action.idempotency_key.len().min(32)];
        let stake_account = Pubkey::create_with_seed(&authority, seed, &stake_program)
            .map_err(|e| AdapterError::Construction(e.to_string()))?;
        let instruction = match action.kind {
            ActionKind::StakeCreate => {
                require_balance(ctx, self.estimate_lamports(action)?)?;
                solana_stake_interface::instruction::create_account_with_seed(
                    &authority,
                    &stake_account,
                    &authority,
                    seed,
                    &solana_stake_interface::state::Authorized::auto(&authority),
                    &solana_stake_interface::state::Lockup::default(),
                    action.amount_lamports,
                )
            }
            ActionKind::StakeDeactivate => {
                vec![solana_stake_interface::instruction::deactivate_stake(
                    &stake_account,
                    &authority,
                )]
            }
            ActionKind::StakeWithdraw => vec![solana_stake_interface::instruction::withdraw(
                &stake_account,
                &authority,
                &authority,
                action.amount_lamports,
                None,
            )],
            other => return Err(AdapterError::UnsupportedAction(other)),
        };
        Ok(instruction
            .into_iter()
            .map(|ix| instruction_spec(ix, "controlled native stake lifecycle action"))
            .collect())
    }

    fn expected_changes(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<ExpectedChange>, AdapterError> {
        let sign = if matches!(action.kind, ActionKind::StakeWithdraw) {
            1
        } else {
            -1
        };
        Ok(vec![ExpectedChange {
            account: ctx.signer_public_key.clone(),
            lamports_delta: sign * action.amount_lamports.min(i64::MAX as u64) as i64,
            token_delta: None,
        }])
    }

    fn safety_classification(&self) -> &'static str {
        "explicit-stake-lifecycle"
    }
}

/// A deliberately inert adapter showing extension without scheduler changes.
#[derive(Debug, Default)]
pub struct ExampleReadOnlyAdapter;

impl ProtocolAdapter for ExampleReadOnlyAdapter {
    fn id(&self) -> &'static str {
        "example-read-only"
    }
    fn validate_configuration(&self, _: &BTreeMap<String, String>) -> Result<(), AdapterError> {
        Ok(())
    }
    fn supported_actions(&self) -> &'static [ActionKind] {
        &[ActionKind::Browse]
    }
    fn required_program_ids(&self) -> Vec<String> {
        vec![MEMO_PROGRAM_ID.into()]
    }
    fn estimate_lamports(&self, _: &PlannedAction) -> Result<u64, AdapterError> {
        Ok(5_000)
    }
    fn build_instructions(
        &self,
        action: &PlannedAction,
        ctx: &AdapterContext,
    ) -> Result<Vec<InstructionSpec>, AdapterError> {
        MemoAdapter.build_instructions(action, ctx)
    }
    fn expected_changes(
        &self,
        _: &PlannedAction,
        _: &AdapterContext,
    ) -> Result<Vec<ExpectedChange>, AdapterError> {
        Ok(Vec::new())
    }
    fn safety_classification(&self) -> &'static str {
        "metadata-only"
    }
}

pub fn default_adapters() -> Vec<Box<dyn ProtocolAdapter>> {
    vec![
        Box::new(NativeSolAdapter),
        Box::new(MemoAdapter),
        Box::new(SplTokenAdapter),
        Box::new(NativeStakeAdapter),
    ]
}

fn parse_key(value: &str) -> Result<Pubkey, AdapterError> {
    Pubkey::from_str(value).map_err(|_| AdapterError::Construction("invalid public key".into()))
}

fn parse_counterparty(ctx: &AdapterContext) -> Result<Pubkey, AdapterError> {
    parse_key(
        ctx.counterparty_public_key
            .as_deref()
            .ok_or_else(|| AdapterError::Construction("counterparty required".into()))?,
    )
}

fn require_balance(ctx: &AdapterContext, required: u64) -> Result<(), AdapterError> {
    if ctx.available_lamports < required {
        Err(AdapterError::InsufficientBalance {
            required,
            available: ctx.available_lamports,
        })
    } else {
        Ok(())
    }
}

fn instruction_spec(instruction: Instruction, description: &str) -> InstructionSpec {
    InstructionSpec {
        program_id: instruction.program_id.to_string(),
        accounts: instruction
            .accounts
            .into_iter()
            .map(|meta| AccountMetaSpec {
                public_key: meta.pubkey.to_string(),
                signer: meta.is_signer,
                writable: meta.is_writable,
            })
            .collect(),
        data: instruction.data,
        redacted_description: description.into(),
    }
}

fn short_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use solana_pubkey::Pubkey;
    use uuid::Uuid;

    use account_cooker_core::{ExecutionState, PlannerModel};

    use super::*;

    fn action(kind: ActionKind) -> PlannedAction {
        PlannedAction {
            id: Uuid::new_v4(),
            fleet_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            scheduled_at: Utc::now(),
            kind,
            adapter_id: kind.adapter_id().into(),
            amount_lamports: 100_000,
            counterparty: Some(Uuid::new_v4()),
            asset: "SOL".into(),
            state: ExecutionState::Planned,
            idempotency_key: "0123456789abcdef0123456789abcdef".into(),
            model: PlannerModel::PersonaSession,
            seed_tag: "test".into(),
            session_id: None,
        }
    }

    fn context() -> AdapterContext {
        AdapterContext {
            signer_public_key: Pubkey::new_unique().to_string(),
            counterparty_public_key: Some(Pubkey::new_unique().to_string()),
            mint_public_key: Some(Pubkey::new_unique().to_string()),
            available_lamports: 100_000_000,
            available_tokens: 1_000_000,
        }
    }

    #[test]
    fn native_transfer_declares_only_system_program() {
        let adapter = NativeSolAdapter;
        let specs = adapter
            .build_instructions(&action(ActionKind::NativeTransfer), &context())
            .unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].program_id, SYSTEM_PROGRAM_ID);
    }

    #[test]
    fn spl_transfer_builds_idempotent_ata_and_checked_transfer() {
        let specs = SplTokenAdapter
            .build_instructions(&action(ActionKind::SplTokenTransfer), &context())
            .unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[1].program_id, TOKEN_PROGRAM_ID);
    }

    #[test]
    fn insufficient_balance_fails_before_construction() {
        let mut ctx = context();
        ctx.available_lamports = 1;
        assert!(matches!(
            NativeSolAdapter.build_instructions(&action(ActionKind::NativeTransfer), &ctx),
            Err(AdapterError::InsufficientBalance { .. })
        ));
    }
}
