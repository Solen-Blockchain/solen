# Spec 002: State Model

**Status:** Draft

## Canonical State Objects

| Object | Key Fields |
|--------|------------|
| Account | account_id, code_hash, owners, threshold, guardians, nonce, spending_policies, session_keys |
| Validator | validator_id, pubkeys, stake, status, slashing_history, reward_destination |
| Rollup | rollup_id, vm_type, proof_type, da_mode, bridge_config, sequencer_set, governance_params |
| Bridge Vault | asset_id, origin_domain, supply, custody_state, pending_exits, challenge_windows |
| Message Receipt | source, destination, nonce, payload_hash, timeout, proof_reference, execution_status |
