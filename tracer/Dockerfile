FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --bins

FROM debian:bookworm-slim
COPY --from=build /src/target/release/rewind-agent /src/target/release/rewind-collector /usr/local/bin/
ENTRYPOINT ["rewind-agent"]
