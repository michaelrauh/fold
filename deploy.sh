#!/bin/bash
# deploy.sh - Deploy fold to a DigitalOcean droplet
#
# Prerequisites:
# - doctl installed and authenticated
# - SSH key added to DigitalOcean account
#
# Usage: ./deploy.sh

set -xe

echo "=== Deploying fold to DigitalOcean ==="

# Configuration
DROPLET_NAME="fold-runner-$(date +%s)"
DROPLET_SIZE="s-4vcpu-16gb-amd"  # 4 vCPU, 16GB RAM ($84/mo)
DROPLET_IMAGE="ubuntu-22-04-x64"
DROPLET_REGION="nyc1"
SSH_KEY_ID="43081865"  # ed25519 key

# Create droplet
echo "Creating droplet: $DROPLET_NAME"
doctl compute droplet create "$DROPLET_NAME" \
    --size "$DROPLET_SIZE" \
    --image "$DROPLET_IMAGE" \
    --region "$DROPLET_REGION" \
    --ssh-keys "$SSH_KEY_ID" \
    --wait

# Get droplet IP
echo "Getting droplet IP..."
DROPLET_IP=$(doctl compute droplet list --format Name,PublicIPv4 --no-header | grep "$DROPLET_NAME" | awk '{print $2}')
echo "Droplet IP: $DROPLET_IP"

# Wait for SSH to be ready
echo "Waiting for SSH to be ready..."
for i in {1..30}; do
    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@$DROPLET_IP "echo ready" 2>/dev/null; then
        echo "SSH is ready!"
        break
    fi
    echo "Attempt $i/30: SSH not ready yet..."
    sleep 10
done

# Create and upload the remote setup script
cat > /tmp/remote_setup.sh << 'REMOTE_SCRIPT'
#!/bin/bash
set -e

echo "=== Setting up environment ==="

# Wait for cloud-init and automatic updates to finish
echo "Waiting for cloud-init to complete..."
cloud-init status --wait || true

echo "Waiting for automatic updates to complete..."
for i in {1..30}; do
    if ! fuser /var/lib/dpkg/lock-frontend >/dev/null 2>&1 && \
       ! fuser /var/lib/apt/lists/lock >/dev/null 2>&1 && \
       ! fuser /var/lib/dpkg/lock >/dev/null 2>&1; then
        echo "Apt lock released"
        break
    fi
    echo "Waiting for apt lock... (attempt $i/30)"
    sleep 10
done

# Install dependencies
apt-get update
apt-get install -y curl git build-essential tmux

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# Clone repository
cd /root
git clone https://github.com/michaelrauh/fold.git
cd fold
git checkout leading

# Download e.txt
echo "Downloading e.txt..."
curl -o e.txt https://www.gutenberg.org/cache/epub/62/pg62.txt

# Clean state
rm -rf fold_state/

# Stage the file
echo "Staging e.txt..."
./stage.sh e.txt "CHAPTER" 20 "./fold_state" --no-prompt

# Build in release mode
echo "Building application..."
/root/.cargo/bin/cargo build --release

# Create tmux runner script
cat > /root/fold/run_in_tmux.sh << 'EOF'
#!/bin/bash
cd /root/fold
/root/.cargo/bin/cargo run --release
EOF
chmod +x /root/fold/run_in_tmux.sh

# Start tmux session and run
echo "Starting application in tmux..."
tmux new-session -d -s fold '/root/fold/run_in_tmux.sh'

echo "=== Setup complete ==="
echo "Application is running in tmux session 'fold'"
echo "To attach: tmux attach -t fold"
echo "To detach: Ctrl+B, then D"
REMOTE_SCRIPT

# Upload and execute remote setup script
echo "Uploading setup script to droplet..."
scp -o StrictHostKeyChecking=no /tmp/remote_setup.sh root@$DROPLET_IP:/root/

echo "Executing setup script on droplet..."
ssh -o StrictHostKeyChecking=no root@$DROPLET_IP "bash /root/remote_setup.sh"

echo ""
echo "=== Deployment Complete ==="
echo "Droplet Name: $DROPLET_NAME"
echo "Droplet IP: $DROPLET_IP"
echo "SSH command: ssh root@$DROPLET_IP"
echo "Attach to tmux: ssh root@$DROPLET_IP -t 'tmux attach -t fold'"
echo ""
echo "To destroy droplet: doctl compute droplet delete $DROPLET_NAME"
