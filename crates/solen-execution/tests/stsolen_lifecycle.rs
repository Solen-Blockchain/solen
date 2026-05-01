//! End-to-end tests for the stSOLEN contract lifecycle.
//!
//! Builds the WASM blob from `examples/contracts/stsolen/` at test startup
//! (incremental — fast after the first build), deploys it against a real
//! `BlockExecutor` + a pre-populated staking system contract, and exercises
//! the deposit / reward-sync / recompound paths end-to-end.
//!
//! These tests cover the full call graph: contract WASM → `queue_call` →
//! executor patch → `execute_staking_call` → on-chain delegation. They're the
//! authoritative check that the executor patch + the contract logic agree.

use std::path::PathBuf;
use std::process::Command;

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_execution::state::StateManager;
use solen_storage::{MemoryStore, StateStore};
use solen_system_contracts::staking::{StakingContract, MIN_VALIDATOR_STAKE};
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;

const STSOLEN_DIR: &str = "../../examples/contracts/stsolen";
const STSOLEN_WASM: &str = "target/wasm32-unknown-unknown/release/solen_stsolen.wasm";

/// Build the stSOLEN WASM if needed and return its bytes.
///
/// `cargo build` is incremental — after the first run this is sub-second.
/// Failing to build is a hard error: there's no point running the test if
/// the contract doesn't compile.
fn build_and_load_wasm() -> Vec<u8> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let contract_dir = manifest_dir.join(STSOLEN_DIR);
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .current_dir(&contract_dir)
        .status()
        .expect("failed to spawn cargo");
    assert!(status.success(), "stsolen WASM build failed");

    let wasm_path = contract_dir.join(STSOLEN_WASM);
    std::fs::read(&wasm_path).expect("read stSOLEN wasm")
}

fn zero_fee_executor() -> BlockExecutor {
    BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    })
}

fn sign_op(kp: &Keypair, executor: &BlockExecutor, op: &mut UserOperation) {
    let msg = executor.operation_signing_message(op);
    op.signature = kp.sign(&msg).to_vec();
}

fn alice_id() -> AccountId {
    let mut id = [0u8; 32];
    id[..4].copy_from_slice(b"alic");
    id
}

fn treasury_id() -> AccountId {
    let mut id = [0u8; 32];
    id[..4].copy_from_slice(b"trea");
    id
}

fn validator_id() -> AccountId {
    let mut v = [0u8; 32];
    v[0] = 0x07;
    v
}

/// Build a store with alice (1 SOLEN), a separate treasury account, and a
/// pre-registered staking validator.
fn setup() -> (MemoryStore, Keypair) {
    let mut store = MemoryStore::new();
    let kp = Keypair::generate();

    {
        let mut sc = StakingContract::new();
        sc.register_validator(validator_id(), MIN_VALIDATOR_STAKE).unwrap();
        sc.save(&mut store);
    }

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: alice_id(),
                balance: 100_000_000, // 1 SOLEN — plenty of headroom for tests.
                auth_methods: vec![AuthMethod::Ed25519 {
                    public_key: kp.public_key(),
                }],
            },
            GenesisAccount {
                id: treasury_id(),
                balance: 0,
                auth_methods: vec![],
            },
            // Native treasury for fee accounting (the executor's fee_config
            // points here by default; even with zero_fee_executor we need it
            // to exist).
            GenesisAccount {
                id: solen_execution::fees::FeeConfig::default().treasury_account,
                balance: 0,
                auth_methods: vec![],
            },
        ],
    )
    .unwrap();

    (store, kp)
}

/// Deploy stSOLEN, return the contract id and the next-nonce alice should use.
fn deploy_stsolen(
    store: &mut MemoryStore,
    executor: &BlockExecutor,
    kp: &Keypair,
    wasm: &[u8],
) -> (AccountId, u64) {
    let mut deploy_op = UserOperation {
        sender: alice_id(),
        nonce: 0,
        actions: vec![Action::Deploy {
            code: wasm.to_vec(),
            salt: [0xCD; 32],
        }],
        max_fee: 5_000_000,
        signature: vec![],
    };
    sign_op(kp, executor, &mut deploy_op);
    let result = executor.execute_block(store, &[deploy_op]);
    assert!(
        result.receipts[0].success,
        "deploy failed: {:?}",
        result.receipts[0]
    );
    let mut contract_id = [0u8; 32];
    contract_id.copy_from_slice(&result.receipts[0].events[0].data);
    (contract_id, 1)
}

/// Build a `Call(target, method, args)` UserOperation, sign, return.
fn call_op(
    executor: &BlockExecutor,
    kp: &Keypair,
    nonce: u64,
    target: AccountId,
    method: &str,
    args: Vec<u8>,
) -> UserOperation {
    let mut op = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![Action::Call {
            target,
            method: method.to_string(),
            args,
        }],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(kp, executor, &mut op);
    op
}

/// Initialize the contract (sets owner, treasury, slash_oracle, defaults).
fn init_contract(
    store: &mut MemoryStore,
    executor: &BlockExecutor,
    kp: &Keypair,
    contract: AccountId,
    nonce: u64,
) {
    // init args: treasury[32] + slash_oracle[32]. Use the dedicated treasury
    // account for the protocol fee target; alice is the slash oracle (so we
    // can `report_slash` from her in later tests).
    let mut args = Vec::with_capacity(64);
    args.extend_from_slice(&treasury_id());
    args.extend_from_slice(&alice_id());
    let op = call_op(executor, kp, nonce, contract, "init", args);
    let r = executor.execute_block(store, &[op]);
    assert!(r.receipts[0].success, "init failed: {:?}", r.receipts[0]);
}

/// Add the validator as operator slot 0, then set op_count = 1.
fn add_operator(
    store: &mut MemoryStore,
    executor: &BlockExecutor,
    kp: &Keypair,
    contract: AccountId,
    starting_nonce: u64,
) {
    // set_operator(index = 0, operator = validator_id)
    let mut args1 = Vec::with_capacity(40);
    args1.extend_from_slice(&0u64.to_le_bytes());
    args1.extend_from_slice(&validator_id());
    let op1 = call_op(executor, kp, starting_nonce, contract, "set_operator", args1);

    // set_op_count(1)
    let args2 = 1u64.to_le_bytes().to_vec();
    let op2 = call_op(
        executor,
        kp,
        starting_nonce + 1,
        contract,
        "set_op_count",
        args2,
    );

    let r = executor.execute_block(store, &[op1, op2]);
    assert!(r.receipts[0].success, "set_operator failed: {:?}", r.receipts[0]);
    assert!(r.receipts[1].success, "set_op_count failed: {:?}", r.receipts[1]);
}

/// Build a contract-storage backing-store key:  `cs/{contract_id}/{inner}`.
fn cs_key(contract: &AccountId, inner: &[u8]) -> Vec<u8> {
    let mut k = b"cs/".to_vec();
    k.extend_from_slice(contract);
    k.push(b'/');
    k.extend_from_slice(inner);
    k
}

fn read_u128_at(store: &MemoryStore, key: &[u8]) -> u128 {
    match store.get(key).ok().flatten() {
        Some(data) if data.len() >= 16 => {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&data[..16]);
            u128::from_le_bytes(buf)
        }
        _ => 0,
    }
}

fn read_u64_at(store: &MemoryStore, key: &[u8]) -> u64 {
    match store.get(key).ok().flatten() {
        Some(data) if data.len() >= 8 => {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&data[..8]);
            u64::from_le_bytes(buf)
        }
        _ => 0,
    }
}

/// Mirror what the consensus engine would do: write the current block height
/// into `__chain_meta__` so system calls (`staking::read_current_epoch`) see
/// the right epoch. Tests that don't run a full engine have to do this by
/// hand; otherwise unbonding never matures because the staking call always
/// reads epoch 0.
fn set_chain_height(store: &mut MemoryStore, height: u64) {
    let mut meta = [0u8; 16];
    meta[..8].copy_from_slice(&height.to_le_bytes());
    store.put(b"__chain_meta__", &meta).unwrap();
}

fn total_pooled(store: &MemoryStore, contract: &AccountId) -> u128 {
    let key = cs_key(contract, b"total_pooled_solen");
    read_u128_at(store, &key)
}

fn total_supply(store: &MemoryStore, contract: &AccountId) -> u128 {
    let key = cs_key(contract, b"total_supply");
    read_u128_at(store, &key)
}

fn stsolen_balance(store: &MemoryStore, contract: &AccountId, account: &AccountId) -> u128 {
    let mut inner = b"bal/".to_vec();
    inner.extend_from_slice(account);
    let key = cs_key(contract, &inner);
    read_u128_at(store, &key)
}

#[test]
fn first_deposit_mints_minus_bootstrap_burn_and_delegates() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // First deposit: 1_000_000 base units.
    let deposit_amount: u128 = 1_000_000;
    let mut op = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: deposit_amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);
    let r = executor.execute_block(&mut store, &[op]);
    assert!(r.receipts[0].success, "deposit failed: {:?}", r.receipts[0]);

    // Pool reflects the full deposit (including the reserve top-up which is
    // still principal backing stSOLEN).
    assert_eq!(total_pooled(&store, &contract), deposit_amount);

    // Total supply = 1_000_000 (1000 burned to dead + 999_000 to alice).
    assert_eq!(total_supply(&store, &contract), deposit_amount);

    // Alice got `deposit_amount - BOOTSTRAP_BURN`.
    let dead_addr = [0xDEu8; 32];
    assert_eq!(stsolen_balance(&store, &contract, &alice_id()), deposit_amount - 1_000);
    assert_eq!(stsolen_balance(&store, &contract, &dead_addr), 1_000);

    // Staking system contract recorded a delegation FROM the contract for
    // (deposit_amount - MIN_FEE_RESERVE).
    let sc = StakingContract::load(&store);
    let expected_delegated = deposit_amount - 10_000;
    assert_eq!(
        sc.delegator_total_stake(&contract),
        expected_delegated,
        "contract should be delegator on staking"
    );

    // Contract account retains exactly MIN_FEE_RESERVE.
    let mgr = StateManager::new(&mut store);
    let acct = mgr.require_account(&contract).unwrap();
    assert_eq!(acct.balance, 10_000, "contract should retain MIN_FEE_RESERVE");
}

#[test]
fn reward_inflow_grows_pool_and_skims_treasury_fee() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // Bootstrap with a deposit so the pool has supply.
    let deposit_amount: u128 = 5_000_000;
    let mut deposit_op = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: deposit_amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut deposit_op);
    let r = executor.execute_block(&mut store, &[deposit_op]);
    assert!(r.receipts[0].success, "deposit failed: {:?}", r.receipts[0]);
    nonce += 1;

    let pool_before_reward = total_pooled(&store, &contract);
    let supply_before_reward = total_supply(&store, &contract);

    // Inject a "reward" by directly crediting the contract's account balance
    // — this models the per-epoch auto-credit the staking module performs in
    // production via `distribute_epoch_rewards_in_executor`. We bypass the
    // epoch-reward path so the test stays small and deterministic.
    let reward: u128 = 100_000;
    {
        let mut mgr = StateManager::new(&mut store);
        let mut acct = mgr.require_account(&contract).unwrap();
        acct.balance += reward;
        mgr.save_account(&acct).unwrap();
    }

    // `poke()` runs sync_rewards. Pool should grow by `reward * (1 - fee)`,
    // and treasury should receive a fee mint in stSOLEN.
    let poke_op = call_op(&executor, &kp, nonce, contract, "poke", vec![]);
    let r = executor.execute_block(&mut store, &[poke_op]);
    assert!(r.receipts[0].success, "poke failed: {:?}", r.receipts[0]);

    let pool_after = total_pooled(&store, &contract);
    let supply_after = total_supply(&store, &contract);

    // Default fee is 10% (1000 bps). Pool gains 90% of the reward.
    let expected_growth = reward * 90 / 100;
    assert_eq!(
        pool_after - pool_before_reward,
        expected_growth,
        "pool should grow by reward minus 10% fee"
    );

    // Treasury minted ~1 fee_solen-worth of stSOLEN at the post-growth rate.
    // fee_solen = 10_000; fee_mint = 10_000 * supply_before / pool_after.
    let fee_solen: u128 = reward / 10;
    let expected_fee_mint = fee_solen * supply_before_reward / pool_after;
    let treasury_bal = stsolen_balance(&store, &contract, &treasury_id());
    assert_eq!(
        treasury_bal, expected_fee_mint,
        "treasury fee mint mismatch"
    );
    assert_eq!(
        supply_after,
        supply_before_reward + expected_fee_mint,
        "supply should grow only by treasury fee mint"
    );
}

#[test]
fn request_crank_claim_full_withdrawal_cycle() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // Deposit 1_000_000 base units. Alice gets 1_000_000 - 1000 stSOLEN.
    let deposit_amount: u128 = 1_000_000;
    let mut deposit_op = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: deposit_amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut deposit_op);
    let r = executor.execute_block_with_height(&mut store, &[deposit_op], 0);
    assert!(r.receipts[0].success, "deposit failed: {:?}", r.receipts[0]);
    nonce += 1;

    // Capture alice's native-SOLEN balance before the withdrawal pays out.
    let alice_native_before = StateManager::new(&mut store)
        .get_balance(&alice_id())
        .unwrap();

    // Request withdrawal of 100_000 stSOLEN (~10% of supply at exchange rate 1).
    let burn: u128 = 100_000;
    let mut args = Vec::with_capacity(16);
    args.extend_from_slice(&burn.to_le_bytes());
    let req_op = call_op(&executor, &kp, nonce, contract, "request_withdrawal", args);
    let r = executor.execute_block_with_height(&mut store, &[req_op], 0);
    assert!(r.receipts[0].success, "request_withdrawal failed: {:?}", r.receipts[0]);
    nonce += 1;

    // Verify queue state. wq_tail = 1, pending_withdrawal_solen = 100_000.
    let wq_tail = read_u64_at(&store, &cs_key(&contract, b"wq_tail"));
    assert_eq!(wq_tail, 1);
    let pending = read_u128_at(&store, &cs_key(&contract, b"pending_withdrawal_solen"));
    assert_eq!(pending, burn, "pool exchange rate is 1.0; owed == burn");

    // Pool & supply both shrink by `burn`. Supply was deposit_amount; now
    // deposit_amount - burn.
    assert_eq!(total_supply(&store, &contract), deposit_amount - burn);
    assert_eq!(total_pooled(&store, &contract), deposit_amount - burn);

    // Crank at epoch 0 (block 0). Queues `STAKING_ADDRESS:undelegate` for
    // the operator with the pending share.
    let crank_op = call_op(&executor, &kp, nonce, contract, "crank_undelegations", vec![]);
    let r = executor.execute_block_with_height(&mut store, &[crank_op], 1);
    assert!(r.receipts[0].success, "crank failed: {:?}", r.receipts[0]);
    nonce += 1;

    // Staking system contract should now show the contract's delegation
    // reduced by `burn`, with a pending undelegation for the same amount.
    let sc = StakingContract::load(&store);
    let staked_now = sc.delegator_total_stake(&contract);
    let expected_staked = (deposit_amount - 10_000) - burn; // delegated less reserve, less crank
    assert_eq!(staked_now, expected_staked, "delegation should drop by `burn`");

    // Claim before eligibility — should fail (block 100 = epoch 1, way too early).
    let mut early_args = Vec::with_capacity(8);
    early_args.extend_from_slice(&0u64.to_le_bytes());
    let early_op = call_op(&executor, &kp, nonce, contract, "claim_withdrawal", early_args.clone());
    let r = executor.execute_block_with_height(&mut store, &[early_op], 100);
    assert!(
        r.receipts[0].success,
        "early claim shouldn't crash, just return err string: {:?}",
        r.receipts[0]
    );
    // Claim still in queue.
    let wq_head = read_u64_at(&store, &cs_key(&contract, b"wq_head"));
    assert_eq!(wq_head, 0, "early claim shouldn't advance wq_head");
    nonce += 1;

    // Advance chain to epoch 8 (block 800) — past `requested_epoch (0) +
    // UNBONDING (7) + 1`. The staking system contract reads epoch from
    // `__chain_meta__`, which the executor doesn't update on its own, so we
    // bump it explicitly here.
    set_chain_height(&mut store, 800);

    // Crank again at epoch 8 to pull matured undelegations into the buffer.
    // The cranker and the claimant are deliberately separate calls because
    // the executor fires native_transfers before queued contract calls;
    // mixing the two in one op would attempt the payout before the matured
    // pull.
    let crank2 = call_op(&executor, &kp, nonce, contract, "crank_undelegations", vec![]);
    let r = executor.execute_block_with_height(&mut store, &[crank2], 800);
    assert!(r.receipts[0].success, "second crank failed: {:?}", r.receipts[0]);
    nonce += 1;

    // Now claim — pays from the buffer that the crank just filled.
    let claim_op = call_op(&executor, &kp, nonce, contract, "claim_withdrawal", early_args);
    let r = executor.execute_block_with_height(&mut store, &[claim_op], 800);
    assert!(r.receipts[0].success, "claim failed: {:?}", r.receipts[0]);

    // Alice's native SOLEN balance should have grown by `burn` (the locked owed).
    let alice_native_after = StateManager::new(&mut store)
        .get_balance(&alice_id())
        .unwrap();
    assert_eq!(
        alice_native_after - alice_native_before,
        burn,
        "alice should receive `burn` SOLEN net after withdrawal"
    );

    // Pending tracker drained.
    let pending_after = read_u128_at(&store, &cs_key(&contract, b"pending_withdrawal_solen"));
    assert_eq!(pending_after, 0);

    // Withdrawal-buffer net should land at zero (matured = burn, paid burn).
    let buffer = read_u128_at(&store, &cs_key(&contract, b"withdrawal_buffer"));
    assert_eq!(buffer, 0);

    // Queue head advanced.
    let wq_head_after = read_u64_at(&store, &cs_key(&contract, b"wq_head"));
    assert_eq!(wq_head_after, 1);

    // Staking-side: undelegation matured + withdrawn. Contract's stake on
    // staking should be back to (delegated - burn), no pending undelegation.
    let sc = StakingContract::load(&store);
    assert_eq!(sc.delegator_total_stake(&contract), expected_staked);
    let pending_undel: u128 = sc
        .undelegations
        .iter()
        .filter(|u| u.delegator == contract)
        .map(|u| u.amount)
        .sum();
    assert_eq!(
        pending_undel, 0,
        "staking should have no pending undelegations after withdraw"
    );
}

#[test]
fn claim_before_eligibility_returns_error() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // Deposit + request + crank, then try to claim immediately.
    let deposit_amount: u128 = 500_000;
    let mut dep = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: deposit_amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep);
    let r = executor.execute_block_with_height(&mut store, &[dep], 0);
    assert!(r.receipts[0].success);
    nonce += 1;

    let mut req_args = Vec::with_capacity(16);
    req_args.extend_from_slice(&100_000u128.to_le_bytes());
    let req = call_op(&executor, &kp, nonce, contract, "request_withdrawal", req_args);
    let r = executor.execute_block_with_height(&mut store, &[req], 0);
    assert!(r.receipts[0].success);
    nonce += 1;

    let crank = call_op(&executor, &kp, nonce, contract, "crank_undelegations", vec![]);
    let r = executor.execute_block_with_height(&mut store, &[crank], 0);
    assert!(r.receipts[0].success);
    nonce += 1;

    // Claim at block 50 (still epoch 0). The contract should reject; not
    // panic, not advance head.
    let mut claim_args = Vec::with_capacity(8);
    claim_args.extend_from_slice(&0u64.to_le_bytes());
    let claim = call_op(&executor, &kp, nonce, contract, "claim_withdrawal", claim_args);
    let r = executor.execute_block_with_height(&mut store, &[claim], 50);
    assert!(r.receipts[0].success, "call shouldn't panic on early claim");

    // wq_head should not have advanced.
    let head = read_u64_at(&store, &cs_key(&contract, b"wq_head"));
    assert_eq!(head, 0);
    let pending = read_u128_at(&store, &cs_key(&contract, b"pending_withdrawal_solen"));
    assert_eq!(pending, 100_000, "pending unchanged on rejected claim");
}

/// Admin methods must be owner-gated. A non-owner caller should hit
/// `err:unauthorized` and leave state untouched.
#[test]
fn admin_methods_reject_non_owner() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;

    // Set up a second account that is NOT the contract owner. Owner is
    // alice (the deployer); mallory is a freshly-keyed account funded from
    // genesis.
    let mallory_id = {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(b"mall");
        id
    };
    let mallory_kp = Keypair::generate();
    {
        let mut mgr = StateManager::new(&mut store);
        mgr.create_account(
            mallory_id,
            vec![AuthMethod::Ed25519 { public_key: mallory_kp.public_key() }],
            10_000_000,
        )
        .unwrap();
    }

    // Snapshot pre-attempt state.
    let owner_before = read_owner(&store, &contract);

    let try_admin = |store: &mut MemoryStore, method: &str, args: Vec<u8>, m_nonce: u64| {
        let mut op = UserOperation {
            sender: mallory_id,
            nonce: m_nonce,
            actions: vec![Action::Call {
                target: contract,
                method: method.to_string(),
                args,
            }],
            max_fee: 1_000_000,
            signature: vec![],
        };
        op.signature = mallory_kp
            .sign(&executor.operation_signing_message(&op))
            .to_vec();
        executor.execute_block(store, &[op])
    };

    // Each admin call should "succeed" at the executor level (the contract
    // returns `err:unauthorized` as a return-value string, not a panic) but
    // leave state unchanged.
    let r = try_admin(&mut store, "pause", vec![], 0);
    assert!(r.receipts[0].success);
    let r = try_admin(&mut store, "set_treasury", mallory_id.to_vec(), 1);
    assert!(r.receipts[0].success);
    let mut bps_args = Vec::with_capacity(8);
    bps_args.extend_from_slice(&1500u64.to_le_bytes());
    let r = try_admin(&mut store, "set_protocol_fee_bps", bps_args, 2);
    assert!(r.receipts[0].success);

    // Owner unchanged.
    assert_eq!(read_owner(&store, &contract), owner_before);
    // Treasury still pointing at the original, not mallory.
    assert_eq!(read_32_at(&store, &cs_key(&contract, b"treasury")), treasury_id());
    // Default fee unchanged at 1000 bps.
    assert_eq!(read_u64_at(&store, &cs_key(&contract, b"protocol_fee_bps")), 1000);
    // paused flag still false.
    let paused = match store.get(&cs_key(&contract, b"paused")).ok().flatten() {
        Some(d) => d.first().copied().unwrap_or(0),
        None => 0,
    };
    assert_eq!(paused, 0);

    let _ = nonce;
}

/// While paused, deposits must fail; SRC-20 transfers must still succeed
/// (the spec carves out `transfer` so users can exit via solenswap even
/// during a halt). Unpause and the deposit path works again.
#[test]
fn pause_halts_deposits_but_not_transfers() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // First, a normal deposit so we have stSOLEN to transfer.
    let mut dep = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: 1_000_000 },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep);
    let r = executor.execute_block(&mut store, &[dep]);
    assert!(r.receipts[0].success);
    nonce += 1;

    // Owner pauses.
    let pause_op = call_op(&executor, &kp, nonce, contract, "pause", vec![]);
    let r = executor.execute_block(&mut store, &[pause_op]);
    assert!(r.receipts[0].success);
    nonce += 1;

    // Deposit should now fail. The op is "successful" at the executor level
    // (the contract returned an error string, not a panic), but the
    // pre-paused balance shouldn't change beyond the queued Transfer (which
    // does land — Transfer is action-level and doesn't go through the
    // contract's pause check). Easier check: confirm total_pooled_solen
    // didn't grow.
    let pool_before = total_pooled(&store, &contract);
    let supply_before = total_supply(&store, &contract);
    let mut dep2 = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: 500_000 },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep2);
    let r = executor.execute_block(&mut store, &[dep2]);
    // The deposit Call returns `err:paused`. The executor sees a successful
    // op (no panic), but the contract didn't mint or change pool state. The
    // bare Transfer DID land — that's an action-level operation.
    assert!(r.receipts[0].success);
    assert_eq!(total_pooled(&store, &contract), pool_before);
    assert_eq!(total_supply(&store, &contract), supply_before);
    nonce += 1;

    // SRC-20 transfer should still succeed while paused. Send 1000 stSOLEN
    // from alice to mallory.
    let mallory_id = {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(b"mall");
        id
    };
    let mut xfer_args = Vec::with_capacity(48);
    xfer_args.extend_from_slice(&mallory_id);
    xfer_args.extend_from_slice(&1000u128.to_le_bytes());
    let xfer_op = call_op(&executor, &kp, nonce, contract, "transfer", xfer_args);
    let r = executor.execute_block(&mut store, &[xfer_op]);
    assert!(r.receipts[0].success);
    assert_eq!(stsolen_balance(&store, &contract, &mallory_id), 1000);
    nonce += 1;

    // Unpause + deposit succeeds.
    let unpause_op = call_op(&executor, &kp, nonce, contract, "unpause", vec![]);
    let r = executor.execute_block(&mut store, &[unpause_op]);
    assert!(r.receipts[0].success);
    nonce += 1;

    let pool_pre = total_pooled(&store, &contract);
    let mut dep3 = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: 500_000 },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep3);
    let r = executor.execute_block(&mut store, &[dep3]);
    assert!(r.receipts[0].success);
    // Pool grew by the new deposit + the orphaned Transfer that landed during
    // the paused attempt (still in the contract's account; sync_rewards on
    // unpaused deposit absorbs it as reward).
    assert!(total_pooled(&store, &contract) > pool_pre);
}

/// Slashing oracle: only the `slash_oracle` key may call `report_slash`.
/// Non-loss reports (`realized >= prior`) must be rejected so an unbonded
/// or buggy oracle can't inflate the pool.
#[test]
fn report_slash_oracle_auth_and_math() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    // Init sets slash_oracle = alice (per init_contract helper).
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // Deposit so op_stake[validator] is populated.
    let deposit_amount: u128 = 1_000_000;
    let mut dep = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: deposit_amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep);
    let r = executor.execute_block(&mut store, &[dep]);
    assert!(r.receipts[0].success);
    nonce += 1;

    // op_stake before any slash report.
    let mut op_stake_inner = b"op_stake/".to_vec();
    op_stake_inner.extend_from_slice(&validator_id());
    let stake_before = read_u128_at(&store, &cs_key(&contract, &op_stake_inner));
    let pool_before = total_pooled(&store, &contract);
    assert!(stake_before > 0);

    // Mallory (not the oracle) tries to report a slash — must fail.
    let mallory_id = {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(b"mall");
        id
    };
    let mallory_kp = Keypair::generate();
    {
        let mut mgr = StateManager::new(&mut store);
        mgr.create_account(
            mallory_id,
            vec![AuthMethod::Ed25519 { public_key: mallory_kp.public_key() }],
            10_000_000,
        )
        .unwrap();
    }
    let mut bad_args = Vec::with_capacity(48);
    bad_args.extend_from_slice(&validator_id());
    bad_args.extend_from_slice(&(stake_before / 2).to_le_bytes());
    let mut bad_op = UserOperation {
        sender: mallory_id,
        nonce: 0,
        actions: vec![Action::Call {
            target: contract,
            method: "report_slash".to_string(),
            args: bad_args,
        }],
        max_fee: 1_000_000,
        signature: vec![],
    };
    bad_op.signature = mallory_kp
        .sign(&executor.operation_signing_message(&bad_op))
        .to_vec();
    let r = executor.execute_block(&mut store, &[bad_op]);
    assert!(r.receipts[0].success); // contract returns err string, not panic
    // State unchanged.
    assert_eq!(
        read_u128_at(&store, &cs_key(&contract, &op_stake_inner)),
        stake_before
    );
    assert_eq!(total_pooled(&store, &contract), pool_before);

    // Alice (the oracle) reports a non-loss — must reject.
    let mut nonloss = Vec::with_capacity(48);
    nonloss.extend_from_slice(&validator_id());
    nonloss.extend_from_slice(&(stake_before + 100).to_le_bytes());
    let nonloss_op = call_op(&executor, &kp, nonce, contract, "report_slash", nonloss);
    let r = executor.execute_block(&mut store, &[nonloss_op]);
    assert!(r.receipts[0].success); // err:not_a_loss
    assert_eq!(
        read_u128_at(&store, &cs_key(&contract, &op_stake_inner)),
        stake_before,
        "non-loss must not increase op_stake"
    );
    assert_eq!(total_pooled(&store, &contract), pool_before);
    nonce += 1;

    // Alice reports a real slash. realized = prior - 50_000.
    let realized = stake_before - 50_000;
    let mut good_args = Vec::with_capacity(48);
    good_args.extend_from_slice(&validator_id());
    good_args.extend_from_slice(&realized.to_le_bytes());
    let good_op = call_op(&executor, &kp, nonce, contract, "report_slash", good_args);
    let r = executor.execute_block(&mut store, &[good_op]);
    assert!(r.receipts[0].success);

    let stake_after = read_u128_at(&store, &cs_key(&contract, &op_stake_inner));
    let pool_after = total_pooled(&store, &contract);
    assert_eq!(stake_after, realized);
    assert_eq!(pool_after, pool_before - 50_000, "pool should decrement by exact loss");
}

/// After a reward inflow grows the pool, a subsequent deposit of the same
/// SOLEN amount mints *fewer* stSOLEN. This is the core economic behavior:
/// the exchange rate has risen.
#[test]
fn exchange_rate_rises_after_reward() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // Bootstrap deposit: 10_000_000. Alice gets 9_999_000 stSOLEN.
    let amount: u128 = 10_000_000;
    let mut dep1 = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep1);
    let r = executor.execute_block(&mut store, &[dep1]);
    assert!(r.receipts[0].success);
    nonce += 1;

    let mint1 = stsolen_balance(&store, &contract, &alice_id());
    assert_eq!(mint1, amount - 1_000); // bootstrap burn

    // Inject 10% reward by direct balance credit, then poke to absorb.
    let reward = amount / 10;
    {
        let mut mgr = StateManager::new(&mut store);
        let mut acct = mgr.require_account(&contract).unwrap();
        acct.balance += reward;
        mgr.save_account(&acct).unwrap();
    }
    let poke_op = call_op(&executor, &kp, nonce, contract, "poke", vec![]);
    let r = executor.execute_block(&mut store, &[poke_op]);
    assert!(r.receipts[0].success);
    nonce += 1;

    // Top alice up enough to deposit again.
    {
        let mut mgr = StateManager::new(&mut store);
        let mut acct = mgr.require_account(&alice_id()).unwrap();
        acct.balance += amount;
        mgr.save_account(&acct).unwrap();
    }

    // Second deposit of the same SOLEN amount.
    let mut dep2 = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep2);
    let r = executor.execute_block(&mut store, &[dep2]);
    assert!(r.receipts[0].success);

    let mint2_total = stsolen_balance(&store, &contract, &alice_id());
    let mint2 = mint2_total - mint1; // alice's incremental mint
    assert!(
        mint2 < mint1,
        "second deposit at higher rate should mint fewer stSOLEN: mint1={mint1}, mint2={mint2}"
    );
}

/// Claiming with seq matching head + eligible epoch but empty buffer must
/// fail with `err:buffer_insufficient` and leave state untouched.
#[test]
fn claim_without_crank_fails_buffer_insufficient() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // Deposit, request, *no* crank.
    let mut dep = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: 1_000_000 },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut dep);
    let r = executor.execute_block(&mut store, &[dep]);
    assert!(r.receipts[0].success);
    nonce += 1;

    let mut req_args = Vec::with_capacity(16);
    req_args.extend_from_slice(&100_000u128.to_le_bytes());
    let req = call_op(&executor, &kp, nonce, contract, "request_withdrawal", req_args);
    let r = executor.execute_block(&mut store, &[req]);
    assert!(r.receipts[0].success);
    nonce += 1;

    // Even at epoch 8, with no crank ever called, the buffer is empty so
    // claim must reject.
    set_chain_height(&mut store, 800);
    let mut claim_args = Vec::with_capacity(8);
    claim_args.extend_from_slice(&0u64.to_le_bytes());
    let claim = call_op(&executor, &kp, nonce, contract, "claim_withdrawal", claim_args);
    let r = executor.execute_block_with_height(&mut store, &[claim], 800);
    assert!(r.receipts[0].success); // contract returns err string

    // wq_head unchanged.
    let head = read_u64_at(&store, &cs_key(&contract, b"wq_head"));
    assert_eq!(head, 0);
    let buffer = read_u128_at(&store, &cs_key(&contract, b"withdrawal_buffer"));
    assert_eq!(buffer, 0);
}

/// Helper: read a 32-byte storage value for a contract.
fn read_32_at(store: &MemoryStore, key: &[u8]) -> [u8; 32] {
    match store.get(key).ok().flatten() {
        Some(data) if data.len() >= 32 => {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&data[..32]);
            buf
        }
        _ => [0u8; 32],
    }
}

fn read_owner(store: &MemoryStore, contract: &AccountId) -> [u8; 32] {
    read_32_at(store, &cs_key(contract, b"owner"))
}

#[test]
fn recompound_redelegates_idle_rewards() {
    let wasm = build_and_load_wasm();
    let (mut store, kp) = setup();
    let executor = zero_fee_executor();

    let (contract, mut nonce) = deploy_stsolen(&mut store, &executor, &kp, &wasm);
    init_contract(&mut store, &executor, &kp, contract, nonce);
    nonce += 1;
    add_operator(&mut store, &executor, &kp, contract, nonce);
    nonce += 2;

    // Bootstrap deposit so the pool exists.
    let deposit_amount: u128 = 50_000_000_000; // 500 SOLEN
    let mut deposit_op = UserOperation {
        sender: alice_id(),
        nonce,
        actions: vec![
            Action::Transfer { to: contract, amount: deposit_amount },
            Action::Call { target: contract, method: "deposit".to_string(), args: vec![] },
        ],
        max_fee: 5_000_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut deposit_op);

    // Alice's balance probably can't cover 500 SOLEN. Top her up first.
    {
        let mut mgr = StateManager::new(&mut store);
        let mut acct = mgr.require_account(&alice_id()).unwrap();
        acct.balance += 100_000_000_000;
        mgr.save_account(&acct).unwrap();
    }
    let r = executor.execute_block(&mut store, &[deposit_op]);
    assert!(r.receipts[0].success, "deposit failed: {:?}", r.receipts[0]);
    nonce += 1;

    // Inject a substantial reward (>= 100 SOLEN to clear the recompound dust
    // floor) by direct balance credit.
    let reward: u128 = 200 * 100_000_000; // 200 SOLEN
    {
        let mut mgr = StateManager::new(&mut store);
        let mut acct = mgr.require_account(&contract).unwrap();
        acct.balance += reward;
        mgr.save_account(&acct).unwrap();
    }

    let sc = StakingContract::load(&store);
    let staked_before = sc.delegator_total_stake(&contract);

    // Bump block height past epoch 0 so the rate-limit doesn't wedge us — we
    // need `current_epoch > last_recompound_epoch` (last is 0 default; we go
    // to epoch 1). Block 100 == epoch 1.
    let recompound_op = call_op(&executor, &kp, nonce, contract, "recompound_rewards", vec![]);
    let r = executor.execute_block_with_height(&mut store, &[recompound_op], 100);
    assert!(
        r.receipts[0].success,
        "recompound failed: {:?}",
        r.receipts[0]
    );

    // Staking should now hold (staked_before + reward - fee_growth_already_in_pool).
    // After sync_rewards: fee_solen = 20 SOLEN; growth_solen = 180 SOLEN; pool +=180 SOLEN.
    // Then recompound delegates available = balance - pending - MIN_FEE_RESERVE.
    // balance after sync (no actual change in balance): contract still has the 200
    // SOLEN reward sitting un-delegated. available ≈ 200 SOLEN - MIN_FEE_RESERVE.
    let sc = StakingContract::load(&store);
    let staked_after = sc.delegator_total_stake(&contract);
    let new_delegation = staked_after - staked_before;
    // Expect: ~200 SOLEN - 10_000 (MIN_FEE_RESERVE).
    assert!(
        new_delegation >= reward - 20_000 && new_delegation <= reward,
        "recompound should re-delegate close to reward; got {new_delegation}, reward {reward}"
    );

    // Contract account should retain MIN_FEE_RESERVE.
    let mgr = StateManager::new(&mut store);
    let acct = mgr.require_account(&contract).unwrap();
    assert_eq!(acct.balance, 10_000, "reserve preserved");
}
