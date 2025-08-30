#!/usr/bin/env bash
set -euo pipefail

# Provision Kubernetes control plane for fold deployment
# Sets up the basic cluster infrastructure without deploying services

NAMESPACE=${NAMESPACE:-fold}

echo "Provisioning Kubernetes control plane for fold..."

# Create namespace
echo "Creating namespace: $NAMESPACE"
kubectl apply -f k8s/namespace.yaml

# Verify cluster is accessible
echo "Verifying cluster access..."
kubectl cluster-info

# Check node readiness
echo "Checking node status..."
kubectl get nodes

echo "Control plane provisioning complete!"
echo "Ready for deployment. Run 'make k8s-deploy' to deploy applications and infrastructure."