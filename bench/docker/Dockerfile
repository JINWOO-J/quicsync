FROM rust:1.88-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    openssh-server openssh-client rsync iproute2 iputils-ping \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /run/sshd /root/.ssh /app/upload-test

# SSH 설정
RUN ssh-keygen -A \
    && echo 'PermitRootLogin yes' >> /etc/ssh/sshd_config \
    && echo 'PubkeyAuthentication yes' >> /etc/ssh/sshd_config \
    && echo 'PasswordAuthentication no' >> /etc/ssh/sshd_config

# client→server SSH 키 생성 (빌드 시 고정)
RUN ssh-keygen -t ed25519 -f /root/.ssh/id_ed25519 -N ""

COPY --from=builder /build/target/release/quicsync /usr/local/bin/quicsync

EXPOSE 22
