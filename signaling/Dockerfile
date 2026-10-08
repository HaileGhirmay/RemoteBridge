# Build from the repository root:
#   docker build -f signaling/Dockerfile -t remotebridge-signaling .
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p rb-signaling --bin rb-signaling-server

FROM debian:bookworm-slim
# No shell access needed, no root.
RUN useradd --system --uid 10001 --no-create-home rb
COPY --from=build /src/target/release/rb-signaling-server /usr/local/bin/rb-signaling-server
USER rb
ENV PORT=8080 RUST_LOG=info
EXPOSE 8080
# SIGNALING_JWT_SECRET must be provided at run time (at least 32 bytes).
ENTRYPOINT ["rb-signaling-server"]
