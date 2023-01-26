# Backrun Example

## Example:
```bash

git submodule update --init --recursive

cargo b --release && \
    RUST_LOG=info ./target/release/jito-backrun-example \
    --auth-addr http://{auth_addr}:1005 \
    --jito_searcher_client-addr http://{searcher_addr}:1004 \
    --payer-keypair id.json \
    --auth-keypair id.json \
    --pubsub-url ws://{RPC_URL}:8900 \
    --rpc-url http://{RPC_URL}:8899 \
    --tip-program-id {TIP_PROGRAM_ID} \
    --backrun-accounts {account}
```
