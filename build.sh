#!/usr/bin/env bash
set -euo pipefail

# Build Docker image for fold application

REGISTRY=${REGISTRY:-registry.digitalocean.com/fold}
IMAGE_NAME=${IMAGE_NAME:-fold}
IMAGE_TAG=${IMAGE_TAG:-latest}
FULL_IMAGE="$REGISTRY/$IMAGE_NAME:$IMAGE_TAG"

echo "Building Docker image for fold application..."

# Build Docker image
echo "Building Docker image: $FULL_IMAGE"
docker build -t fold:latest -f Dockerfile .
docker tag fold:latest "$FULL_IMAGE"

# Push to registry
echo "Pushing image to registry..."
docker push "$FULL_IMAGE"

echo "Build complete!"
echo "Image: $FULL_IMAGE"