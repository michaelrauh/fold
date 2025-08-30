#!/usr/bin/env bash
set -euo pipefail

# Deploy fold application and infrastructure to Kubernetes
# Run after provision.sh and build.sh

REGISTRY=${REGISTRY:-registry.digitalocean.com/fold}
IMAGE_NAME=${IMAGE_NAME:-fold}
IMAGE_TAG=${IMAGE_TAG:-latest}
FULL_IMAGE="$REGISTRY/$IMAGE_NAME:$IMAGE_TAG"
NAMESPACE=${NAMESPACE:-fold}

echo "Deploying fold application and infrastructure to Kubernetes..."

# Ensure namespace exists
echo "Ensuring namespace exists: $NAMESPACE"
kubectl apply -f k8s/namespace.yaml

# Deploy infrastructure first (PostgreSQL, RabbitMQ, MinIO)
echo "Deploying infrastructure components..."

# Deploy PostgreSQL database
echo "Deploying PostgreSQL database..."
kubectl apply -f - <<EOF
apiVersion: apps/v1
kind: Deployment
metadata:
  name: postgres
  namespace: $NAMESPACE
spec:
  replicas: 1
  selector:
    matchLabels:
      app: postgres
  template:
    metadata:
      labels:
        app: postgres
    spec:
      containers:
      - name: postgres
        image: postgres:13
        env:
        - name: POSTGRES_DB
          value: fold
        - name: POSTGRES_USER
          value: fold
        - name: POSTGRES_PASSWORD
          value: foldpass
        ports:
        - containerPort: 5432
        volumeMounts:
        - name: postgres-storage
          mountPath: /var/lib/postgresql/data
      volumes:
      - name: postgres-storage
        emptyDir: {}
---
apiVersion: v1
kind: Service
metadata:
  name: postgres
  namespace: $NAMESPACE
spec:
  selector:
    app: postgres
  ports:
  - port: 5432
    targetPort: 5432
EOF

# Deploy RabbitMQ message queue
echo "Deploying RabbitMQ message queue..."
kubectl apply -f - <<EOF
apiVersion: apps/v1
kind: Deployment
metadata:
  name: rabbitmq
  namespace: $NAMESPACE
spec:
  replicas: 1
  selector:
    matchLabels:
      app: rabbitmq
  template:
    metadata:
      labels:
        app: rabbitmq
    spec:
      containers:
      - name: rabbitmq
        image: rabbitmq:3-management
        env:
        - name: RABBITMQ_DEFAULT_USER
          value: fold
        - name: RABBITMQ_DEFAULT_PASS
          value: foldpass
        ports:
        - containerPort: 5672
        - containerPort: 15672
---
apiVersion: v1
kind: Service
metadata:
  name: rabbitmq
  namespace: $NAMESPACE
spec:
  selector:
    app: rabbitmq
  ports:
  - name: amqp
    port: 5672
    targetPort: 5672
  - name: management
    port: 15672
    targetPort: 15672
EOF

# Deploy MinIO object storage
echo "Deploying MinIO object storage..."
kubectl apply -f - <<EOF
apiVersion: apps/v1
kind: Deployment
metadata:
  name: minio
  namespace: $NAMESPACE
spec:
  replicas: 1
  selector:
    matchLabels:
      app: minio
  template:
    metadata:
      labels:
        app: minio
    spec:
      containers:
      - name: minio
        image: minio/minio
        args:
        - server
        - /data
        - --console-address
        - ":9001"
        env:
        - name: MINIO_ROOT_USER
          value: minioadmin
        - name: MINIO_ROOT_PASSWORD
          value: minioadmin
        ports:
        - containerPort: 9000
        - containerPort: 9001
        volumeMounts:
        - name: minio-storage
          mountPath: /data
      volumes:
      - name: minio-storage
        emptyDir: {}
---
apiVersion: v1
kind: Service
metadata:
  name: minio
  namespace: $NAMESPACE
spec:
  selector:
    app: minio
  ports:
  - name: api
    port: 9000
    targetPort: 9000
  - name: console
    port: 9001
    targetPort: 9001
EOF

# Create configuration secrets and configmaps
echo "Creating configuration secrets and config maps..."
kubectl apply -f - <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: fold-secrets
  namespace: $NAMESPACE
type: Opaque
stringData:
  FOLD_PG_URL: "postgresql://fold:foldpass@postgres:5432/fold"
  FOLD_AMQP_URL: "amqp://fold:foldpass@rabbitmq:5672/"
  minio-access-key: "minioadmin"
  minio-secret-key: "minioadmin"
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: fold-config
  namespace: $NAMESPACE
data:
  FOLD_INTERNER_BLOB_ENDPOINT: "http://minio:9000"
  FOLD_INTERNER_BLOB_BUCKET: "internerdata"
  FOLD_INTERNER_FILE_LOCATION: "/tmp/interner"
EOF

# Wait for infrastructure to be ready
echo "Waiting for infrastructure to be ready..."
kubectl wait --for=condition=ready pod -l app=postgres -n $NAMESPACE --timeout=300s || echo "Postgres may still be starting..."
kubectl wait --for=condition=ready pod -l app=rabbitmq -n $NAMESPACE --timeout=300s || echo "RabbitMQ may still be starting..."
kubectl wait --for=condition=ready pod -l app=minio -n $NAMESPACE --timeout=300s || echo "MinIO may still be starting..."

# Initialize MinIO bucket
echo "Initializing MinIO bucket..."
kubectl run mc-client --rm -i --restart=Never --image=minio/mc --namespace=$NAMESPACE -- sh -c "
mc alias set localminio http://minio:9000 minioadmin minioadmin
mc mb localminio/internerdata || echo 'Bucket already exists'
mc policy set public localminio/internerdata
" || echo "MinIO bucket initialization may have failed, continuing..."

# Deploy application components
echo "Deploying application components..."
for f in k8s/*-deployment.yaml; do
    echo "Applying $f..."
    # Use sed to replace the image tag (simpler than yq)
    sed "s|image: fold:latest|image: $FULL_IMAGE|g" "$f" | kubectl apply -f -
done

echo "Deployment complete!"
echo "Infrastructure deployed:"
echo "  Database: postgres:5432"
echo "  Message Queue: rabbitmq:5672"
echo "  Object Storage: minio:9000"
echo "Application components deployed from k8s/ manifests"
echo "Check status with: kubectl get pods -n $NAMESPACE"