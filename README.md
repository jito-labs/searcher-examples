# About

# Program
An example program that backruns accounts with a 1 lamport transfer and a memo program.

## Example:
```bash

git submodule update --init --recursive

cargo b --release && \
    RUST_LOG=info ./target/release/jito-backrun-example \
    --auth-addr http://{auth_addr}:1005 \
    --searcher-addr http://{searcher_addr}:1004 \
    --payer-keypair id.json \
    --auth-keypair id.json \
    --pubsub-url ws://{RPC_URL}:8900 \
    --rpc-url http://{RPC_URL}:8899 \
    --tip-program-id {TIP_PROGRAM_ID} \
    --backrun-accounts {account}
```

## Connecting
Please check out https://jito-labs.gitbook.io/mev/systems/connecting for the most up-to-date information on block engines.
Please check out https://jito-foundation.gitbook.io/mev/mev-payment-and-distribution/on-chain-addresses for the most up-to-date tip program id.

## Disclaimer
Use this at your own risk. 
