# Backrun Example

See the [cli README](../cli/README.md) for setup instructions.

## Usage

```bash
cargo run --bin jito-backrun-example -- \
    --block-engine-url <BLOCK_ENGINE_URL> \
    --payer-keypair <PAYER_KEYPAIR> \
    --auth-keypair <AUTH_KEYPAIR> \
    --pubsub-url ws://{RPC_URL}:8900 \
    --rpc-url http://{RPC_URL}:8899 \
    --tip-program-id <TIP_PROGRAM_ID> \
    --backrun-accounts <BACKRUN_ACCOUNTS>
    # Note: Don't provide --auth-keypair argument if not planning to use authentication
```

## Example

Backrun transactions that write-lock the [Pyth SOL/USDC account](https://solscan.io/account/H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG):

```bash
RUST_LOG=INFO cargo run --bin jito-backrun-example -- \
--block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
--payer-keypair keypair.json \
--auth-keypair keypair.json \
--pubsub-url ws://api.mainnet-beta.solana.com:8900 \
--rpc-url https://api.mainnet-beta.solana.com:8899 \
--tip-program-id T1pyyaTNZsKv2WcRAB8oVnk93mLJw2XzjtVYqCsaHqt \
--backrun-accounts H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG
# Note: Don't provide --auth-keypair argument if not planning to use authentication
```
