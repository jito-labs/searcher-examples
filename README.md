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
This is a locally generated wallet which you will use to sign transactions and pay for transaction fees and tips with.

### Block Engine URLs
A list of our block engine URLs can be found here: https://jito-labs.gitbook.io/mev/systems/connecting/mainnet

### Block Engine API Key
Please apply for block engine API keys here: https://web.miniextensions.com/WV3gZjFwqNqITsMufIEp

### On-chain addresses
On-chain addresses for tip programs and tip accounts can be found here: https://jito-foundation.gitbook.io/mev/mev-payment-and-distribution/on-chain-addresses

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
