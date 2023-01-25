# About
This library contains tooling to interact with Jito Lab's Block Engine as a searcher.

# Tooling

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
$ poetry protoc
$ poetry build
$ poetry publish
```
