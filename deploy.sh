#!/usr/bin/env bash
set -euo pipefail

# Deploy fold application to Kubernetes
# Run after provision.sh and build.sh

REGISTRY=${REGISTRY:-registry.digitalocean.com/fold}
IMAGE_NAME=${IMAGE_NAME:-fold}
IMAGE_TAG=${IMAGE_TAG:-latest}
FULL_IMAGE="$REGISTRY/$IMAGE_NAME:$IMAGE_TAG"
NAMESPACE=${NAMESPACE:-fold}

echo "Deploying fold application to Kubernetes..."

# Ensure namespace exists
echo "Ensuring namespace exists: $NAMESPACE"
kubectl apply -f k8s/namespace.yaml

# Update image in deployment files and apply
echo "Applying deployment manifests..."
for f in k8s/*-deployment.yaml; do
    echo "Applying $f..."
    # Use sed to replace the image tag (simpler than yq)
    sed "s|image: fold:latest|image: $FULL_IMAGE|g" "$f" | kubectl apply -f -
done

echo "Deployment complete!"
echo "Check status with: kubectl get pods -n $NAMESPACE"