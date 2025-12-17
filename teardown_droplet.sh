#!/usr/bin/env bash
set -euo pipefail

# Teardown helper to delete the droplet and block volume.

DROPLET_NAME="${DROPLET_NAME:-fold-16gb}"
VOLUME_NAME="${VOLUME_NAME:-fold-data}"
REGION="${REGION:-nyc3}"

abort() { echo "error: $*" >&2; exit 1; }

find_volume_id() {
  doctl compute volume list --region "$REGION" --format ID,Name --no-header 2>/dev/null |
    awk -v name="$VOLUME_NAME" '$2 == name {print $1}'
}

delete_droplet() {
  if doctl compute droplet get "$DROPLET_NAME" >/dev/null 2>&1; then
    echo "Deleting droplet $DROPLET_NAME..."
    doctl compute droplet delete "$DROPLET_NAME" --force
  else
    echo "Droplet $DROPLET_NAME not found (skipping)."
  fi
}

wait_for_detach() {
  local volume_id="$1"
  local tries=12
  local attachments
  for i in $(seq 1 "$tries"); do
    attachments="$(doctl compute volume get "$volume_id" --format DropletIDs --no-header 2>/dev/null || true)"
    if [ -z "$attachments" ] || [ "$attachments" = "[]" ]; then
      return 0
    fi
    echo "Volume still attached to droplet(s) $attachments (attempt $i/$tries); waiting..."
    sleep 5
  done
  abort "Volume $volume_id still attached after waiting"
}

delete_volume() {
  local id
  id="$(find_volume_id || true)"
  if [ -z "$id" ]; then
    echo "Volume $VOLUME_NAME not found in $REGION (skipping)."
    return
  fi

  wait_for_detach "$id"
  echo "Deleting volume $VOLUME_NAME ($id)..."
  doctl compute volume delete "$id" --force
}

main() {
  delete_droplet
  delete_volume
}

main "$@"
