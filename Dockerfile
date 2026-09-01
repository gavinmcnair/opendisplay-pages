# Runs egham_ble in a Linux container with its own BlueZ stack -- built for
# TrueNAS SCALE with a USB Bluetooth dongle (ASUS BT-series), but nothing
# here is TrueNAS-specific. See docker-compose.yml for the required runtime
# settings (host networking is NOT optional -- Bluetooth HCI sockets are
# network-namespaced) and README's deployment section for host-side checks.

FROM rust:1-bookworm AS build
WORKDIR /src
# Dependency layer first so code-only changes don't re-download crates.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src
COPY fonts ./fonts
COPY src ./src
# Touch main.rs so cargo notices the real sources replaced the stub.
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
# bluez + dbus: btleplug talks to BlueZ over D-Bus, and the container runs
# its own bluetoothd (the TrueNAS host doesn't run one) -- see entrypoint.sh.
# tzdata: the scheduler's trains window is Europe/London wall-clock time
# (chrono::Local reads TZ). ca-certificates: HTTPS roots for the API fetches.
RUN apt-get update \
    && apt-get install -y --no-install-recommends bluez dbus tzdata ca-certificates \
    && rm -rf /var/lib/apt/lists/*
ENV TZ=Europe/London
COPY --from=build /src/target/release/egham_ble /usr/local/bin/egham_ble
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh && mkdir /data
# State fingerprints (egham_state_slot*.txt) are written to the working
# directory -- /data is the volume mount point so they survive restarts.
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
