# About
This library contains tooling to interact with Jito Lab's Block Engine as a searcher.

# Downloading
```bash
$ pip install jito_searcher_client
```

# Keypair Authentication
Please request access to the block engine by creating a solana keypair and emailing the public key to support@jito.wtf.

# Simple Example

```python
from jito_searcher_client import get_searcher_client
from jito_searcher_client.generated.searcher_pb2 import ConnectedLeadersRequest

from solders.keypair import Keypair

KEYPAIR_PATH = "/path/to/authenticated/keypair.json"
BLOCK_ENGINE_URL = "frankfurt.mainnet.block-engine.jito.wtf"

with open(KEYPAIR_PATH) as kp_path:
    kp = Keypair.from_json(kp_path.read())

client = get_searcher_client(BLOCK_ENGINE_URL, kp)
leaders = client.GetConnectedLeaders(ConnectedLeadersRequest())
print(f"{leaders=}")
```

# Development

Install pip
```bash
$ curl -sSL https://bootstrap.pypa.io/get-pip.py | python 3 -
```

Install poetry
```bash
$ curl -sSL https://install.python-poetry.org | python3 -
```

Setup environment and build protobufs
```bash
$ poetry install
$ poetry protoc
$ poetry shell
```

Publishing package
```bash
$ poetry protoc && poetry build && poetry publish
```
