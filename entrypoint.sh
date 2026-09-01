#!/bin/sh
# Bring up the container's own D-Bus + BlueZ before egham_ble: btleplug
# needs a running bluetoothd reachable over the system bus, and the TrueNAS
# host doesn't provide one. The hci0 device itself comes from the HOST
# kernel (btusb + the dongle's firmware load there) -- this script can start
# bluetoothd but cannot conjure the adapter; if hci0 never appears, check
# the host, not the container (see README's deployment section).
set -eu

mkdir -p /run/dbus
rm -f /run/dbus/pid
dbus-daemon --system --fork

bluetoothd &

# Wait for the adapter (up to 30s). A cold dongle can take a few seconds to
# enumerate; failing fast with a clear message beats btleplug's generic
# "no adapter found" after it does.
i=0
while [ ! -d /sys/class/bluetooth/hci0 ]; do
    i=$((i + 1))
    if [ "$i" -gt 30 ]; then
        echo "hci0 never appeared -- is the dongle plugged in, and does the HOST kernel show it? (dmesg | grep -i bluetooth)" >&2
        exit 1
    fi
    sleep 1
done

# Make sure the adapter is powered -- bluetoothd doesn't always bring a
# freshly-enumerated adapter up on its own.
sleep 1
bluetoothctl power on || true

exec /usr/local/bin/egham_ble "$@"
