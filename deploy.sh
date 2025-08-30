#!/usr/bin/env bash
set -euo pipefail

# Deploy fold application to Kubernetes
# Run after provision.sh and build.sh
# Assumes infrastructure (PostgreSQL, RabbitMQ, MinIO) is already provisioned via Helm

REGISTRY=${REGISTRY:-registry.digitalocean.com/fold}
IMAGE_NAME=${IMAGE_NAME:-fold}
IMAGE_TAG=${IMAGE_TAG:-latest}
FULL_IMAGE="$REGISTRY/$IMAGE_NAME:$IMAGE_TAG"
NAMESPACE=${NAMESPACE:-fold}

echo "Deploying fold application to Kubernetes..."

# Ensure namespace exists
echo "Ensuring namespace exists: $NAMESPACE"
kubectl apply -f k8s/namespace.yaml

# Wait for infrastructure to be ready (assuming it was provisioned)
echo "Checking if infrastructure is ready..."
kubectl wait --for=condition=ready pod -l app.kubernetes.io/instance=postgres-k -n default --timeout=60s || echo "PostgreSQL may still be starting..."
kubectl wait --for=condition=ready pod -l app.kubernetes.io/instance=rabbit-k -n default --timeout=60s || echo "RabbitMQ may still be starting..."
kubectl wait --for=condition=ready pod -l app.kubernetes.io/instance=minio-k -n default --timeout=60s || echo "MinIO may still be starting..."

# Initialize MinIO bucket if needed
echo "Ensuring MinIO bucket exists..."
kubectl run mc-client --rm -i --restart=Never --image=minio/mc --namespace=default -- sh -c "
mc alias set localminio http://minio-k.default.svc.cluster.local:9000 minioadmin minioadmin
mc mb localminio/internerdata || echo 'Bucket already exists'
mc policy set public localminio/internerdata
" || echo "MinIO bucket initialization may have failed, continuing..."

# Deploy application components
echo "Deploying application components..."
for f in k8s/*-deployment.yaml; do
    if [ -f "$f" ]; then
        echo "Applying $f..."
        # Use sed to replace the image tag (simpler than yq)
        sed "s|image: fold:latest|image: $FULL_IMAGE|g" "$f" | kubectl apply -f -
    fi
done

echo "Deployment complete!"
echo "Infrastructure provided by Helm charts:"
echo "  Database: postgres-k-postgresql.default.svc.cluster.local:5432"
echo "  Message Queue: rabbit-k-rabbitmq.default.svc.cluster.local:5672"
echo "  Object Storage: minio-k.default.svc.cluster.local:9000"
echo "Application components deployed from k8s/ manifests"
echo "Check status with: kubectl get pods -n $NAMESPACE"