# syntax=docker/dockerfile:1.4.0
FROM rust:1.66.0-slim-bullseye as builder

RUN set -x \
    && apt-get -qq update \
    && apt-get -qq -y install \
    clang \
    cmake \
    libudev-dev \
    unzip \
    libssl-dev \
    pkg-config \
    zlib1g-dev \
    curl
RUN rustup component add rustfmt && update-ca-certificates

ENV PROTOC_VERSION 21.12
ENV PROTOC_ZIP protoc-$PROTOC_VERSION-linux-x86_64.zip
RUN curl -Ss -OL https://github.com/google/protobuf/releases/download/v$PROTOC_VERSION/$PROTOC_ZIP \
 && unzip -o $PROTOC_ZIP -d /usr/local bin/protoc \
 && unzip -o $PROTOC_ZIP -d /usr/local include/* \
 && rm -f $PROTOC_ZIP

ENV HOME=/home/root
WORKDIR $HOME/app
COPY . .

# cache these directories for reuse
# see: https://docs.docker.com/build/cache/#use-the-dedicated-run-cache
RUN --mount=type=cache,mode=0777,target=/home/root/app/target \
    --mount=type=cache,mode=0777,target=/usr/local/cargo/registry \
    --mount=type=cache,mode=0777,target=/usr/local/cargo/git \
    RUSTFLAGS="-C target-cpu=native" cargo build --release && cp target/release/jito-* ./

FROM debian:bullseye-slim as base_image
RUN apt-get -qq update && apt-get -qq -y install ca-certificates libssl1.1 && rm -rf /var/lib/apt/lists/*


FROM base_image as searcher_cli
ENV APP="jito-searcher-cli"
WORKDIR /app
COPY --from=builder /home/root/app/${APP} ./
ENTRYPOINT ["/app/jito-searcher-cli"]


FROM base_image as searcher_client
ENV APP="jito-searcher-client"
WORKDIR /app
COPY --from=builder /home/root/app/${APP} ./
ENTRYPOINT ["/app/jito-searcher-client"]


FROM base_image as backrun
ENV APP="jito-backrun-example"
WORKDIR /app
COPY --from=builder /home/root/app/${APP} ./
ENTRYPOINT ["/app/jito-backrun-example"]
