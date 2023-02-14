# Backrun Example

## Example:
```bash

git submodule update --init --recursive

cargo b --release && \
    RUST_LOG=info ./target/release/jito-backrun-example \
    --block-engine-addr <BLOCK_ENGINE_ADDR> \
    --payer-keypair <PAYER_KEYPAIR> \
    --auth-keypair <AUTH_KEYPAIR> \
    --pubsub-url ws://{RPC_URL}:8900 \
    --rpc-url http://{RPC_URL}:8899 \
    --tip-program-id {TIP_PROGRAM_ID} \
    --backrun-accounts {account}
```
