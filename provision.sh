#!/usr/bin/env bash
set -euo pipefail

# Provision DOKS cluster - ultra-simple like polyvinyl-acetate
echo "Provisioning DOKS cluster..."

# Create the DOKS cluster and add registry access
if doctl kubernetes cluster get fold-cluster >/dev/null 2>&1; then
	echo "Cluster 'fold-cluster' already exists; skipping create"
else
	echo "Creating DOKS cluster 'fold-cluster'..."
	doctl kubernetes cluster create fold-cluster --size s-8vcpu-16gb-intel --count 1 --wait
fi

echo "Adding registry access to cluster..."
doctl kubernetes cluster registry add fold-cluster

echo "DOKS cluster provisioning complete!"
echo "Ready for build and deployment. Run 'make k8s-build-deploy' to build and deploy everything."