FROM debian:bookworm-slim

ARG TARGETARCH

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl bzip2 ca-certificates \
    && case "${TARGETARCH}" in \
        amd64) archive=monero-linux-x64-v0.18.5.1.tar.bz2; checksum=22a7dda7b0cb699fdd6b7674c3b4a4465b337cc98a54983523b759e1e7cc9958 ;; \
        arm64) archive=monero-linux-armv8-v0.18.5.1.tar.bz2; checksum=c0caf042cb7c7b760f5ad6be188084b59352440b32990a78b8051497b9398dbc ;; \
        *) exit 1 ;; \
    esac \
    && curl --fail --location --retry 3 "https://downloads.getmonero.org/cli/${archive}" -o /tmp/monero.tar.bz2 \
    && printf '%s  /tmp/monero.tar.bz2\n' "${checksum}" | sha256sum --check - \
    && mkdir /tmp/monero \
    && tar -xjf /tmp/monero.tar.bz2 -C /tmp/monero --strip-components=1 \
    && install /tmp/monero/monerod /tmp/monero/monero-wallet-rpc /usr/local/bin/ \
    && monero-wallet-rpc --help | grep -q -- '--allow-mismatched-daemon-version' \
    && rm -rf /tmp/monero /tmp/monero.tar.bz2 /var/lib/apt/lists/*