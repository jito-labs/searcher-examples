

# About
foo

# Tooling

Install pip
```asm
$ curl -sSL https://bootstrap.pypa.io/get-pip.py | python 3 -
```

Install virtualenv
```bash
$ pip3 install virtualenv
```

Install poetry
```bash
$ curl -sSL https://install.python-poetry.org | python3 -
```

# How to develop
```bash
$ poetry install
$ poetry protoc
```

# Helpful poetry commands

Activate poetry shell and exit
```bash
$ poetry shell
$ exit
```

Install poetry packages
```bash
$ poetry install
```

Build protobufs
```bash
$ poetry protoc
```

Package library
```bash
$ poetry build
```

Publishing to PyPI
```bash
$ poetry publish
```
