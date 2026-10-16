FROM rust:1-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config perl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p tutabridge

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends libdbus-1-3 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home tutabridge \
    && mkdir /config \
    && chown tutabridge /config
COPY --from=build /src/target/release/tutabridge /usr/local/bin/tutabridge

ENV XDG_CONFIG_HOME=/config \
    TUTABRIDGE_BIND_ADDRESS=0.0.0.0 \
    TUTABRIDGE_SESSION_STORE=file
USER tutabridge
VOLUME /config
EXPOSE 1143 1025
ENTRYPOINT ["tutabridge"]
