# Searcher CLI

## About

The following program exposes functionality in the Block Engine's searcher API.

## Setup

- Ensure the rust compiler is installed.
- Sending a bundle requires an RPC server and a keypair with funds to pay for tip + transaction fees.
- For cross region functionality, add the `--regions REGION1,REGION2,etc` arg. [More details](https://jito-labs.gitbook.io/mev/searcher-services/recommendations#cross-region)

## Building

```bash
git submodule update --init --recursive
cargo b --release --bin jito-searcher-cli
```

## Running

### Get the next scheduled leader

Returns the pubkey of the next scheduled leader.

```bash
cargo run --bin jito-searcher-cli -- \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  next-scheduled-leader
  # Note: Skip `--keypair-path` argument if not planning to use authentication
```

Example output:

```bash
NextScheduledLeaderResponse { current_slot: 197084695, next_leader_slot: 197084788, next_leader_identity: "5pPRHniefFjkiaArbGX3Y8NUysJmQ9tMZg3FrFGwHzSm" }
```

### Get connected leaders

Returns the [validators](https://jito-foundation.gitbook.io/mev/solana-mev/systems#jito-solana) connected to Block
Engine as map of Pubkey to scheduled leader slots.

```bash
cargo run --bin jito-searcher-cli -- \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  connected-leaders
  # Note: Don't provide --keypair-path argument if not planning to use authentication
```

Example output:

```bash
ConnectedLeadersResponse { connected_validators: {"CquA9q57TYVr9uvXvk6aqAG5GGKk3mUL9C8ALyAsUeWg": SlotList { slots: [196992512, 196992513, <snipped>] } } }
```

### Get tip payment accounts

Returns the
current [tip payment accounts](https://jito-foundation.gitbook.io/mev/mev-payment-and-distribution/on-chain-addresses)
that are in use.

```bash
cargo run --bin jito-searcher-cli -- \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  tip-accounts
  # Note: Don't provide --keypair-path argument if not planning to use authentication
```

Example output:

```bash
GetTipAccountsResponse { accounts: ["DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL", <snipped>] }
```

### Send a bundle

Sends a [bundle](https://jito-labs.gitbook.io/mev/searcher-resources/bundles) to Block Engine to be included in next
leader slot.

```bash
cargo run --bin jito-searcher-cli -- \
  --block-engine-url https://frankfurt.mainnet.block-engine.jito.wtf \
  --keypair-path auth.json \
  send-bundle \
  --payer payer.json \
  --message "im testing jito bundles right now this is pretty sick bro" \
  --num-txs 5 \
  --lamports 100000 \
  --tip-account 96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5 \
  --rpc-url "https://mainnet.rpc.jito.wtf/?access-token=<token here>"
  # Note: Don't provide --keypair-path argument if not planning to use authentication
```

Example output:

```bash
Bundle sent. UUID: "f193340e2cd4a5994f8c845d6cdbdd49c508d3f3958a20462aa3f54fb9376e6b"
Waiting for 5 seconds to hear results...
bundle results: BundleResult { bundle_id: "f193340e2cd4a5994f8c845d6cdbdd49c508d3f3958a20462aa3f54fb9376e6b", result: Some(Accepted(Accepted { slot: 197085505, validator_identity: "AaapDdocMdZQaMAF1gXqKX2ixd7YYSxTpKHMcsbcF318" })) }
bundle results: BundleResult { bundle_id: "f193340e2cd4a5994f8c845d6cdbdd49c508d3f3958a20462aa3f54fb9376e6b", result: Some(Accepted(Accepted { slot: 197085507, validator_identity: "AaapDdocMdZQaMAF1gXqKX2ixd7YYSxTpKHMcsbcF318" })) }
Bundle landed successfully
https://solscan.io/tx/5gN8AznZosVvt7oc5hmFqUg6LxygPLjix9fP6Q5ANLy1hiHAqMWeXva68Z4j1XDMBNJZ8n9bQppsCUGAabT73dcY
https://solscan.io/tx/2KqpS57XCPPBDSU2mCWttYpJ8Mp74bXRoLwJ9Cb6Xb6BC6Vtgqjz8o9RDqEXF2t2jNEzrDQqU8pzXDg58zfz9s8T
https://solscan.io/tx/dBy3yu7pAv3J17BvR9gjgvEawh4Vu1QWqA7dkgL69QtjQABo2ru4zmqr9KqRSi4iBbCsu92oygGxpF3btLW8tBJ
https://solscan.io/tx/2ioZ6sSE1RWkrVaqNZErfLBnFbu46ZcaTUgJUvXWToBVdiNG9owbgrBTxEWiCUki6PFrnnENJ8SukQbQLNpUUjqr
https://solscan.io/tx/2E1HoQuZYLoVP2Z3Ct25JQpEJeK7Kphbx6m3mPxBRHEJG9dZ2uUHWVbtccSjDv75t5uJZ5K7182ZrmtMF4PR2yPC
```
