#!/usr/bin/env bash
set -euo pipefail

# Script to provision a 16 GB DigitalOcean droplet, sync the repo and e.txt,
# and prep the box with Rust + tmux. Requires doctl authenticated with an SSH
# key fingerprint or ID available to --ssh-keys.

# --- Config (override via env) ---
DROPLET_NAME="${DROPLET_NAME:-fold-16gb}"
REGION="${REGION:-nyc3}"
SIZE="${SIZE:-s-4vcpu-16gb}"
IMAGE="${IMAGE:-ubuntu-22-04-x64}"
SSH_KEY="${SSH_KEY:-<ssh-key-id-or-fingerprint>}" # must exist in DO + locally loaded
REMOTE_APP_DIR="${REMOTE_APP_DIR:-/root/fold}"
TMUX_SESSION="${TMUX_SESSION:-fold}"

# Code sync: set SYNC_MODE=local to rsync the working tree; SYNC_MODE=github to clone/pull.
SYNC_MODE="${SYNC_MODE:-local}"                # local|github
LOCAL_REPO_PATH="${LOCAL_REPO_PATH:-$PWD}"     # only used when SYNC_MODE=local
GITHUB_REPO="${GITHUB_REPO:-git@github.com:USER/REPO.git}"
BRANCH="${BRANCH:-main}"

# e.txt sync: TEXT_MODE=local to scp a local file; TEXT_MODE=gutenberg to fetch/strip remotely.
TEXT_MODE="${TEXT_MODE:-local}" # local|gutenberg
LOCAL_TEXT_PATH="${LOCAL_TEXT_PATH:-$PWD/e.txt}"
GUTENBERG_URL="${GUTENBERG_URL:-https://www.gutenberg.org/cache/epub/1661/pg1661.txt}"

abort() { echo "error: $*" >&2; exit 1; }

create_or_get_droplet() {
  if doctl compute droplet get "$DROPLET_NAME" >/dev/null 2>&1; then
    echo "Droplet $DROPLET_NAME already exists."
  else
    echo "Creating droplet $DROPLET_NAME ($SIZE in $REGION)..."
    doctl compute droplet create "$DROPLET_NAME" \
      --region "$REGION" \
      --size "$SIZE" \
      --image "$IMAGE" \
      --ssh-keys "$SSH_KEY" \
      --tag-names fold \
      --enable-monitoring \
      --wait
  fi
}

fetch_ip() {
  doctl compute droplet get "$DROPLET_NAME" --format PublicIPv4 --no-header |
    tr -d '[:space:]'
}

wait_for_ssh() {
  local ip="$1"
  echo -n "Waiting for SSH on $ip"
  for i in {1..30}; do
    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 "root@$ip" "echo ok" >/dev/null 2>&1; then
      echo " -> ready"
      return 0
    fi
    echo -n "."
    sleep 5
  done
  echo
  abort "SSH did not become ready"
}

bootstrap_remote() {
  local ip="$1"
  ssh "root@$ip" <<'EOF'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
wait_for_dpkg() {
  while fuser /var/lib/dpkg/lock-frontend >/dev/null 2>&1 || fuser /var/lib/dpkg/lock >/dev/null 2>&1; do
    echo "dpkg locked, waiting..."
    sleep 5
  done
}
apt_with_retry() {
  local tries=5
  local i
  for i in $(seq 1 "$tries"); do
    if "$@"; then
      return 0
    fi
    echo "apt command failed (attempt $i/$tries), retrying in 5s..."
    sleep 5
    wait_for_dpkg
  done
  echo "apt command failed after $tries attempts" >&2
  return 1
}
wait_for_dpkg
apt_with_retry apt-get update -y
apt_with_retry apt-get install -y git tmux build-essential pkg-config libssl-dev curl ca-certificates
if [ ! -d "$HOME/.cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
EOF
}

ensure_remote_dir() {
  local ip="$1"
  ssh "root@$ip" "mkdir -p \"$REMOTE_APP_DIR\""
}

sync_code() {
  local ip="$1"
  case "$SYNC_MODE" in
    local)
      rsync -az --delete --exclude target --exclude .git "$LOCAL_REPO_PATH"/ "root@$ip:$REMOTE_APP_DIR/"
      ;;
    github)
      ssh "root@$ip" <<EOF
set -euo pipefail
if [ ! -d "$REMOTE_APP_DIR/.git" ]; then
  git clone "$GITHUB_REPO" "$REMOTE_APP_DIR"
fi
cd "$REMOTE_APP_DIR"
git fetch origin "$BRANCH"
git checkout "$BRANCH"
git pull --ff-only origin "$BRANCH"
EOF
      ;;
    *)
      abort "Unknown SYNC_MODE: $SYNC_MODE"
      ;;
  esac
}

sync_text() {
  local ip="$1"
  case "$TEXT_MODE" in
    local)
      scp "$LOCAL_TEXT_PATH" "root@$ip:$REMOTE_APP_DIR/e.txt"
      ;;
    gutenberg)
      ssh "root@$ip" <<EOF
set -euo pipefail
cd "$REMOTE_APP_DIR"
curl -L "$GUTENBERG_URL" -o e_raw.txt
sed -n '/^\*\*\* START OF THE PROJECT GUTENBERG EBOOK/,/^\*\*\* END OF THE PROJECT GUTENBERG EBOOK/p' e_raw.txt |
  sed '1d;$d' > e.txt
EOF
      ;;
    *)
      abort "Unknown TEXT_MODE: $TEXT_MODE"
      ;;
  esac
}

main() {
  create_or_get_droplet
  ip="$(fetch_ip)"
  [ -n "$ip" ] || abort "Could not get droplet IP"
  wait_for_ssh "$ip"
  bootstrap_remote "$ip"
  ensure_remote_dir "$ip"
  sync_code "$ip"
  sync_text "$ip"

  echo
  echo "Droplet ready at: $ip"
  echo "SSH in: ssh root@$ip"
  echo "Start tmux: ssh root@$ip \"tmux new -s $TMUX_SESSION\""
  echo "Run app inside tmux: source ~/.cargo/env && cd $REMOTE_APP_DIR && cargo run --release"
}

main "$@"
