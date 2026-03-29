# Solen Deployment

Deployment configurations for running Solen network infrastructure.

## Testnet

The `testnet/` directory contains everything needed to run a public testnet:

| File | Purpose |
|------|---------|
| `genesis.json` | Chain parameters, validators, initial allocations |
| `solen-node.service` | systemd unit for the validator node |
| `solen-faucet.service` | systemd unit for the faucet HTTP service |
| `nginx.conf` | Reverse proxy for public endpoints |
| `setup.sh` | Automated deployment script |

### Public Endpoints

| Service | URL | Port (internal) |
|---------|-----|-----------------|
| JSON-RPC | `https://rpc.solenchain.com` | 19944 |
| Faucet | `https://faucet.solenchain.com` | 8080 |
| Explorer API | `https://api.solenchain.com` | 19955 |
| Explorer | `https://explorer.solenchain.com` | (solenscan) |
| P2P | `seed1.solenchain.com:40333` | 40333 |

### Quick Deploy

```bash
# On the server
git clone <repo> /home/solen/solen
cd /home/solen/solen
./deploy/testnet/setup.sh
sudo systemctl start solen-node solen-faucet
```

### Get Testnet Tokens

```bash
curl -X POST https://faucet.solenchain.com/drip \
  -H "Content-Type: application/json" \
  -d '{"account": "myaccount"}'
```

Or with the CLI:

```bash
solen --rpc https://rpc.solenchain.com key generate mykey
# Then request tokens at faucet.solenchain.com
```

### DNS Records

| Record | Type | Value |
|--------|------|-------|
| `rpc.solenchain.com` | A | `<server IP>` |
| `faucet.solenchain.com` | A | `<server IP>` |
| `api.solenchain.com` | A | `<server IP>` |
| `seed1.solenchain.com` | A | `<server IP>` |
| `seed2.solenchain.com` | A | `<server 2 IP>` |
