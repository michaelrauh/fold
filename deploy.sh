#!/usr/bin/env bash
set -euo pipefail

# Simple Docker build and Kubernetes deployment script
# Replaces the complex build_prod.sh with a simplified version

REGISTRY=${REGISTRY:-registry.digitalocean.com/fold}
IMAGE_NAME=${IMAGE_NAME:-fold}
IMAGE_TAG=${IMAGE_TAG:-latest}
FULL_IMAGE="$REGISTRY/$IMAGE_NAME:$IMAGE_TAG"
NAMESPACE=${NAMESPACE:-fold}

echo "Building and deploying fold application to Kubernetes..."

# Build and push Docker image (simple version without buildx complexity)
echo "Building Docker image: $FULL_IMAGE"
docker build -t "$FULL_IMAGE" -f Dockerfile .
docker push "$FULL_IMAGE"

# Create namespace
echo "Creating namespace: $NAMESPACE"
kubectl apply -f k8s/namespace.yaml

# Update image in deployment files and apply
echo "Deploying to Kubernetes..."
for f in k8s/*-deployment.yaml; do
    echo "Applying $f..."
    # Use sed to replace the image tag (simpler than yq)
    sed "s|image: fold:latest|image: $FULL_IMAGE|g" "$f" | kubectl apply -f -
done

echo "Deployment complete!"
echo "Check status with: kubectl get pods -n $NAMESPACE"