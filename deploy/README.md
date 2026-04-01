# Solen Deployment

Deployment configurations for running Solen network infrastructure.

## Subdomain Scheme

| Network | RPC | Faucet | API | P2P Seeds |
|---------|-----|--------|-----|-----------|
| **Mainnet** | `rpc.solenchain.io` | — | `api.solenchain.io` | `seed1.solenchain.io` |
| **Testnet** | `testnet-rpc.solenchain.io` | `testnet-faucet.solenchain.io` | `testnet-api.solenchain.io` | `testnet-seed1.solenchain.io` |

## Testnet

The `testnet/` directory contains everything needed to run a 4-validator public testnet:

| File | Purpose |
|------|---------|
| `genesis.json` | Chain parameters, 4 validators, faucet config |
| `solen-node.service` | systemd unit for validator-1 (seed node) |
| `solen-faucet.service` | systemd unit for the faucet HTTP service |
| `nginx.conf` | Reverse proxy for public endpoints |
| `setup.sh` | Seed node deployment (server 1 — validator-1, RPC, faucet, nginx) |
| `setup-validator.sh` | Additional validator deployment (servers 2, 3 & 4) |

### Deploy — Server 1 (Seed Node + Validator 1)

```bash
git clone <repo> /home/solen/solen
cd /home/solen/solen
./deploy/testnet/setup.sh
sudo systemctl start solen-node solen-faucet
```

### Deploy — Servers 2, 3 & 4 (Validators)

```bash
git clone <repo> /home/solen/solen
cd /home/solen/solen

# Server 2
./deploy/testnet/setup-validator.sh 2
sudo systemctl start solen-node

# Server 3
./deploy/testnet/setup-validator.sh 3
sudo systemctl start solen-node

# Server 4
./deploy/testnet/setup-validator.sh 4
sudo systemctl start solen-node
```

These validators connect to `testnet-seed1.solenchain.io` and join consensus automatically. No nginx or faucet needed on these servers.

### Validator Topology

| Server | Role | Validator | Can be offline? |
|--------|------|-----------|-----------------|
| 1 | Seed node + RPC + Faucet | validator-1 | No (seed) |
| 2 | Validator | validator-2 | Yes (1 of 4) |
| 3 | Validator | validator-3 | Yes (1 of 4) |
| 4 | Validator | validator-4 | Yes (1 of 4) |

With 4 validators, the network tolerates 1 node offline while maintaining 3/4 (75%) quorum for finality.

### Get Testnet Tokens

```bash
curl -X POST https://testnet-faucet.solenchain.io/drip \
  -H "Content-Type: application/json" \
  -d '{"account": "myaccount"}'
```

Or with the CLI:

```bash
solen --rpc https://testnet-rpc.solenchain.io key generate mykey
# Then request tokens at testnet-faucet.solenchain.io
```

### DNS Records

| Record | Type | Value |
|--------|------|-------|
| `testnet-rpc.solenchain.io` | A | `<server 1 IP>` |
| `testnet-faucet.solenchain.io` | A | `<server 1 IP>` |
| `testnet-api.solenchain.io` | A | `<server 1 IP>` |
| `testnet-seed1.solenchain.io` | A | `<server 1 IP>` |
