#!/usr/bin/env bash
set -euo pipefail

# Simple start script like polyvinyl-acetate
echo "Starting complete fold k8s deployment..."

./provision.sh && ./build-deploy.sh

echo "Deployment complete! Run 'make k8s-monitor' to check status."