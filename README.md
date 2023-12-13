# About
This repository contains a few examples on how to use a searcher.

## Setup
1. Install protoc to compile protobufs
- Follow instructions at: https://grpc.io/docs/protoc-installation/#install-pre-compiled-binaries-any-os
2. Ensure that the rust toolchain is installed
```bash
$ curl https://sh.rustup.rs -sSf | sh
$ source $HOME/.cargo/env
```
3. Clone the repo
```bash
$ git clone https://github.com/jito-labs/searcher-examples.git --recurse-submodules
$ cd searcher-examples
```

## Infrastructure
You will need access to a few things before running these examples.

### Local keypair
This is a locally generated wallet which you will use to sign transactions. Our block-engine is API-gated, checking to make sure your keypair is present, so make sure the keypair you sign transactions with has the same as the pubkey you put into our systems when generating the block engine API key

### Block Engine URLs
A list of our block engine URLs can be found here: https://jito-labs.gitbook.io/mev/systems/connecting/mainnet

### Block Engine API Key
Our block engine is currently API-gated, but we can generate API keys. Please email support@jito.wtf or create a ticket in our discord asking for an API key.

### On-chain addresses
On-chain addresses for tip programs and tip accounts can be found here: https://jito-foundation.gitbook.io/mev/mev-payment-and-distribution/on-chain-addresses

### RPC + Geyser
If one needs access to low-latency, load-balanced RPC and Geyser, please reach out to support@jito.wtf or create a ticket in our discord asking for a Geyser/RPC API key.

## Folders

### backrun
Our most complex example, this shows how to "backrun" transactions. 

Backrunning is when one inserts a transaction immediately behind a target transaction. Common forms of backrunning can be arbitrage and liquidations.

This example listens to transactions from the mempool and submits bundles containing a backrun transaction immediately behind a target transaction.

### cli
This is a rust program that exercises functionality inside the searcher API so you can explore the functionality. It provides an intuitive CLI-based interface for connecting to the block engine and sending test bundles.

### jito_protos
An example on how to build the protobufs that define the messages and services one can use to talk to our block engine.

### searcher_client
An example on how to authenticate with the block engine as a searcher. All users in the block engine need to perform a challenge-response 

## Disclaimer
Use this at your own risk.
