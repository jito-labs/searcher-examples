# Searcher CLI

## About
The following program exposes functionality in the Block Engine's searcher API.

## Setup
- Ensure the rust compiler is installed.
- Ensure you have access to the block engine. Ask in discord or email support@jito.wtf for a token.
- Sending a bundle requires an RPC server and a keypair with funds to pay for tip + transaction fees. If you need access to low latency RPC servers, email support@jito.wtf or create a ticket in our discord. 

## Building
```bash
git submodule update --init --recursive
cargo b --release --bin jito-searcher-cli
```

## Running

### Listening to mempool
Subscribe to transactions that write-lock the Pyth SOL/USDC account (H6Ar...QJEG):
```bash
./target/release/jito-searcher-cli \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  mempool-accounts H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG
```

### Printing out the next scheduled leader
```bash
./target/release/jito-searcher-cli \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  next-scheduled-leader
```

### Getting the connected leaders
```bash
./target/release/jito-searcher-cli \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  connected-leaders
```

### Getting tip accounts
```bash
./target/release/jito-searcher-cli \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  tip-accounts
```

### Sending a bundle
```bash
./target/release/jito-searcher-cli \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  send-bundle \
  --payer payer.json \
  --message "im testing jito bundles right now this is pretty sick bro" \
  --num-txs 5 \
  --lamports 100000 \
  --tip-account 96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5 \
  --rpc-url "https://mainnet.rpc.jito.wtf/?access-token=<token here>"
```
