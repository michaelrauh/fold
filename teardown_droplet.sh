#!/usr/bin/env bash
set -euo pipefail

# Teardown helper to delete the droplet and block volume.

# Load .env if present (gitignored)
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

if [ -z "${SPACES_BUCKET:-}" ] && [ -n "${FOLD_OFFLOAD_SPACES_BUCKET:-}" ]; then
  SPACES_BUCKET="$FOLD_OFFLOAD_SPACES_BUCKET"
fi
if [ -z "${SPACES_REGION:-}" ] && [ -n "${FOLD_OFFLOAD_SPACES_REGION:-}" ]; then
  SPACES_REGION="$FOLD_OFFLOAD_SPACES_REGION"
fi
if [ -z "${SPACES_ENDPOINT:-}" ] && [ -n "${FOLD_OFFLOAD_SPACES_ENDPOINT:-}" ]; then
  SPACES_ENDPOINT="$FOLD_OFFLOAD_SPACES_ENDPOINT"
fi
if [ -z "${SPACES_ACCESS_KEY:-}" ] && [ -n "${FOLD_OFFLOAD_SPACES_ACCESS_KEY:-}" ]; then
  SPACES_ACCESS_KEY="$FOLD_OFFLOAD_SPACES_ACCESS_KEY"
fi
if [ -z "${SPACES_SECRET_KEY:-}" ] && [ -n "${FOLD_OFFLOAD_SPACES_SECRET_KEY:-}" ]; then
  SPACES_SECRET_KEY="$FOLD_OFFLOAD_SPACES_SECRET_KEY"
fi

DROPLET_NAME="${DROPLET_NAME:-fold-16gb}"
VOLUME_NAME="${VOLUME_NAME:-fold-data}"
REGION="${REGION:-nyc3}"
SPACES_BUCKET="${SPACES_BUCKET:-fold-offload}"
SPACES_REGION="${SPACES_REGION:-nyc3}"
SPACES_ENDPOINT="${SPACES_ENDPOINT:-https://$SPACES_REGION.digitaloceanspaces.com}"
SPACES_ACCESS_KEY="${SPACES_ACCESS_KEY:-}"
SPACES_SECRET_KEY="${SPACES_SECRET_KEY:-}"

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

delete_spaces_bucket() {
  if [ -z "$SPACES_ACCESS_KEY" ] || [ -z "$SPACES_SECRET_KEY" ]; then
    echo "Spaces creds not provided; skipping bucket delete"
    return
  fi
  echo "Deleting Spaces bucket $SPACES_BUCKET (if exists)..."
  AWS_ACCESS_KEY_ID="$SPACES_ACCESS_KEY" AWS_SECRET_ACCESS_KEY="$SPACES_SECRET_KEY" AWS_DEFAULT_REGION="$SPACES_REGION" \
    aws --endpoint-url "$SPACES_ENDPOINT" s3 rb "s3://$SPACES_BUCKET" --force >/dev/null 2>&1 || true
}

main() {
  delete_droplet
  delete_volume
  delete_spaces_bucket
}

main "$@"
