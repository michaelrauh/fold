#!/usr/bin/env bash
set -euo pipefail

# Provision a DigitalOcean Space for fold's shared S3 state.
# Requires the AWS CLI configured with Spaces keys (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY).

SPACE_NAME="${SPACE_NAME:-fold-shared}"
SPACES_REGION="${SPACES_REGION:-nyc3}"
PREFIX="${FOLD_S3_PREFIX:-fold}"
ENDPOINT="https://${SPACES_REGION}.digitaloceanspaces.com"

err() { echo "error: $*" >&2; exit 1; }

command -v aws >/dev/null 2>&1 || err "aws CLI not found. Install with: pip install awscli"
[ -n "${AWS_ACCESS_KEY_ID:-}" ] || err "AWS_ACCESS_KEY_ID is not set (use your Spaces access key)"
[ -n "${AWS_SECRET_ACCESS_KEY:-}" ] || err "AWS_SECRET_ACCESS_KEY is not set (use your Spaces secret key)"

if aws --endpoint-url "$ENDPOINT" s3api head-bucket --bucket "$SPACE_NAME" >/dev/null 2>&1; then
  echo "Space ${SPACE_NAME} already exists in ${SPACES_REGION}"
else
  echo "Creating Space ${SPACE_NAME} in ${SPACES_REGION}..."
  aws --endpoint-url "$ENDPOINT" s3api create-bucket --bucket "$SPACE_NAME" >/dev/null
fi

echo "Enabling versioning..."
aws --endpoint-url "$ENDPOINT" s3api put-bucket-versioning \
  --bucket "$SPACE_NAME" \
  --versioning-configuration Status=Enabled >/dev/null

echo "Seeding prefixes (${PREFIX}/input, leases, heartbeats)..."
aws --endpoint-url "$ENDPOINT" s3api put-object --bucket "$SPACE_NAME" --key "${PREFIX}/input/.keep" --body /dev/null >/dev/null
aws --endpoint-url "$ENDPOINT" s3api put-object --bucket "$SPACE_NAME" --key "${PREFIX}/leases/.keep" --body /dev/null >/dev/null

cat <<EOF

Space ready.
Export these when running workers:
  export FOLD_S3_BUCKET=${SPACE_NAME}
  export FOLD_S3_REGION=${SPACES_REGION}
  export FOLD_S3_PREFIX=${PREFIX}
  export FOLD_S3_ENDPOINT=${ENDPOINT}
  export FOLD_WORKER_ID="\$(hostname)-\$\$"

EOF
