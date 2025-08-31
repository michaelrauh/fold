#!/usr/bin/env bash
set -euo pipefail

# Combined build and deploy script like polyvinyl-acetate
# Installs infrastructure, builds images, and deploys everything

REGISTRY=${REGISTRY:-registry.digitalocean.com/fold}
IMAGE_NAME=${IMAGE_NAME:-fold}
IMAGE_TAG=${IMAGE_TAG:-latest}
FULL_IMAGE="$REGISTRY/$IMAGE_NAME:$IMAGE_TAG"
NAMESPACE=${NAMESPACE:-fold}

echo "Combined build and deploy for fold application..."

# Use simple passwords for development - production should use proper secrets management
POSTGRES_PASSWORD=${POSTGRES_PASSWORD:-"foldpass"}
RABBIT_PASSWORD=${RABBIT_PASSWORD:-"foldpass"}
MINIO_ROOT_USER=${MINIO_ROOT_USER:-"minioadmin"}
MINIO_ROOT_PASSWORD=${MINIO_ROOT_PASSWORD:-"minioadmin"}

# Install infrastructure via Helm charts (like polyvinyl-acetate)
echo "Installing infrastructure charts..."
helm repo add bitnami https://charts.bitnami.com/bitnami 2>/dev/null || true
helm repo update

helm upgrade --install postgres-k bitnami/postgresql \
	--namespace default \
	--set auth.username=fold \
	--set auth.password="$POSTGRES_PASSWORD" \
	--set auth.database=fold \
	--set auth.postgresPassword="$POSTGRES_PASSWORD" \
	--wait --timeout=5m

helm upgrade --install rabbit-k bitnami/rabbitmq \
	--namespace default \
	--set auth.username="user" \
	--set auth.password="$RABBIT_PASSWORD" \
	--wait --timeout=3m

helm upgrade --install minio-k bitnami/minio \
	--namespace default \
	--set auth.rootUser="$MINIO_ROOT_USER" \
	--set auth.rootPassword="$MINIO_ROOT_PASSWORD" \
	--wait --timeout=3m

# Build Docker image
echo "Building Docker image: $FULL_IMAGE"
docker build -t fold:latest -f Dockerfile .
docker tag fold:latest "$FULL_IMAGE"

# Push to registry
echo "Pushing image to registry..."
docker push "$FULL_IMAGE"

# Ensure namespace exists
echo "Creating namespace: $NAMESPACE"
kubectl create namespace $NAMESPACE --dry-run=client -o yaml | kubectl apply -f -

# Initialize MinIO bucket if needed
echo "Ensuring MinIO bucket exists..."
kubectl run mc-client --rm -i --restart=Never --image=minio/mc --namespace=default -- sh -c "
mc alias set localminio http://minio-k.default.svc.cluster.local:9000 $MINIO_ROOT_USER $MINIO_ROOT_PASSWORD
mc mb localminio/internerdata || echo 'Bucket already exists'
mc policy set public localminio/internerdata
" || echo "MinIO bucket initialization may have failed, continuing..."

# Deploy application components with simple inline YAML (like polyvinyl-acetate)
echo "Deploying application components..."

kubectl apply -f - <<EOF
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: fold-ingestor
  namespace: $NAMESPACE
spec:
  replicas: 1
  selector:
    matchLabels:
      app: fold-ingestor
  template:
    metadata:
      labels:
        app: fold-ingestor
    spec:
      containers:
      - name: ingestor
        image: $FULL_IMAGE
        command: ["/app/ingestor"]
        env:
        - name: FOLD_AMQP_URL
          value: "amqp://user:$RABBIT_PASSWORD@rabbit-k-rabbitmq.default.svc.cluster.local:5672/"
        - name: FOLD_PG_URL
          value: "postgresql://fold:$POSTGRES_PASSWORD@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
        - name: FOLD_INTERNER_BLOB_ENDPOINT
          value: "http://minio-k.default.svc.cluster.local:9000"
        - name: FOLD_INTERNER_BLOB_BUCKET
          value: "internerdata"
        - name: FOLD_INTERNER_BLOB_ACCESS_KEY
          value: "$MINIO_ROOT_USER"
        - name: FOLD_INTERNER_BLOB_SECRET_KEY
          value: "$MINIO_ROOT_PASSWORD"
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: fold-worker
  namespace: $NAMESPACE
spec:
  replicas: 1
  selector:
    matchLabels:
      app: fold-worker
  template:
    metadata:
      labels:
        app: fold-worker
    spec:
      containers:
      - name: fold-worker
        image: $FULL_IMAGE
        command: ["/app/fold_worker"]
        env:
        - name: FOLD_AMQP_URL
          value: "amqp://user:$RABBIT_PASSWORD@rabbit-k-rabbitmq.default.svc.cluster.local:5672/"
        - name: FOLD_PG_URL
          value: "postgresql://fold:$POSTGRES_PASSWORD@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
        - name: FOLD_INTERNER_BLOB_ENDPOINT
          value: "http://minio-k.default.svc.cluster.local:9000"
        - name: FOLD_INTERNER_BLOB_BUCKET
          value: "internerdata"
        - name: FOLD_INTERNER_BLOB_ACCESS_KEY
          value: "$MINIO_ROOT_USER"
        - name: FOLD_INTERNER_BLOB_SECRET_KEY
          value: "$MINIO_ROOT_PASSWORD"
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: fold-feeder
  namespace: $NAMESPACE
spec:
  replicas: 1
  selector:
    matchLabels:
      app: fold-feeder
  template:
    metadata:
      labels:
        app: fold-feeder
    spec:
      containers:
      - name: feeder
        image: $FULL_IMAGE
        command: ["/app/feeder"]
        env:
        - name: FOLD_AMQP_URL
          value: "amqp://user:$RABBIT_PASSWORD@rabbit-k-rabbitmq.default.svc.cluster.local:5672/"
        - name: FOLD_PG_URL
          value: "postgresql://fold:$POSTGRES_PASSWORD@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
        - name: FOLD_INTERNER_BLOB_ENDPOINT
          value: "http://minio-k.default.svc.cluster.local:9000"
        - name: FOLD_INTERNER_BLOB_BUCKET
          value: "internerdata"
        - name: FOLD_INTERNER_BLOB_ACCESS_KEY
          value: "$MINIO_ROOT_USER"
        - name: FOLD_INTERNER_BLOB_SECRET_KEY
          value: "$MINIO_ROOT_PASSWORD"
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: fold-follower
  namespace: $NAMESPACE
spec:
  replicas: 1
  selector:
    matchLabels:
      app: fold-follower
  template:
    metadata:
      labels:
        app: fold-follower
    spec:
      containers:
      - name: follower
        image: $FULL_IMAGE
        command: ["/app/follower"]
        env:
        - name: FOLD_AMQP_URL
          value: "amqp://user:$RABBIT_PASSWORD@rabbit-k-rabbitmq.default.svc.cluster.local:5672/"
        - name: FOLD_PG_URL
          value: "postgresql://fold:$POSTGRES_PASSWORD@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
        - name: FOLD_INTERNER_BLOB_ENDPOINT
          value: "http://minio-k.default.svc.cluster.local:9000"
        - name: FOLD_INTERNER_BLOB_BUCKET
          value: "internerdata"
        - name: FOLD_INTERNER_BLOB_ACCESS_KEY
          value: "$MINIO_ROOT_USER"
        - name: FOLD_INTERNER_BLOB_SECRET_KEY
          value: "$MINIO_ROOT_PASSWORD"
EOF

echo "Build and deployment complete!"
echo "Infrastructure provided by Helm charts:"
echo "  Database: postgres-k-postgresql.default.svc.cluster.local:5432"
echo "  Message Queue: rabbit-k-rabbitmq.default.svc.cluster.local:5672"  
echo "  Object Storage: minio-k.default.svc.cluster.local:9000"
echo "Application components deployed to namespace: $NAMESPACE"
echo "Check status with: kubectl get pods -n $NAMESPACE"