#!/usr/bin/env bash
set -euo pipefail

# Simple cleanup script like polyvinyl-acetate
echo "Cleaning up fold k8s deployment..."

# Delete the entire cluster
doctl kubernetes cluster delete fold-cluster -f

echo "Cleanup complete!"