#!/usr/bin/env bash
set -euo pipefail

# Provision DOKS cluster and infrastructure for fold deployment
# Sets up the cluster and infrastructure services via Helm charts

NAMESPACE=${NAMESPACE:-fold}

echo "Provisioning DOKS cluster and infrastructure for fold..."

# Create the DOKS cluster and wait until it is ready; if it exists, continue
if doctl kubernetes cluster get fold-cluster >/dev/null 2>&1; then
	echo "Cluster 'fold-cluster' already exists; skipping create"
else
	echo "Creating DOKS cluster 'fold-cluster'..."
	doctl kubernetes cluster create fold-cluster --size s-8vcpu-16gb-intel --count 1 --wait
fi

echo "Configuring DigitalOcean Container Registry access for cluster 'fold-cluster'..."

# Ensure required tools are present
if ! command -v doctl >/dev/null 2>&1; then
	echo "ERROR: 'doctl' is required and not installed. Install doctl and re-run." >&2
	exit 1
fi

# Ensure kube namespace exists before applying secrets
kubectl create namespace $NAMESPACE --dry-run=client -o yaml | kubectl apply -f -

# Generate the DOCR secret manifest (this must succeed)
doctl registry kubernetes-manifest fold > /tmp/docr-secret.yaml
echo "Applying DOCR Kubernetes secret manifest"
kubectl apply -f /tmp/docr-secret.yaml

# Copy registry secrets into fold namespace
echo "Copying registry secrets into namespace '$NAMESPACE'"
if kubectl get secret fold -n kube-system >/dev/null 2>&1; then
	kubectl get secret fold -n kube-system -o yaml \
		| sed "s/namespace: kube-system/namespace: $NAMESPACE/" \
		| kubectl apply -f -
fi

echo "Adding/updating helm repo bitnami..."
helm repo add bitnami https://charts.bitnami.com/bitnami 2>/dev/null || true
helm repo update

echo "Installing/upgrading infrastructure charts (postgres, rabbitmq, minio) into 'default' namespace"

# Use simple passwords for development - production should use proper secrets management
POSTGRES_PASSWORD=${POSTGRES_PASSWORD:-"foldpass"}
RABBIT_PASSWORD=${RABBIT_PASSWORD:-"foldpass"}
MINIO_ROOT_USER=${MINIO_ROOT_USER:-"minioadmin"}
MINIO_ROOT_PASSWORD=${MINIO_ROOT_PASSWORD:-"minioadmin"}

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

# Create configuration secrets and configmaps for application use
echo "Creating application configuration secrets and config maps..."
kubectl apply -f - <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: fold-secrets
  namespace: $NAMESPACE
type: Opaque
stringData:
  FOLD_PG_URL: "postgresql://fold:$POSTGRES_PASSWORD@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
  FOLD_AMQP_URL: "amqp://user:$RABBIT_PASSWORD@rabbit-k-rabbitmq.default.svc.cluster.local:5672/"
  minio-access-key: "$MINIO_ROOT_USER"
  minio-secret-key: "$MINIO_ROOT_PASSWORD"
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: fold-config
  namespace: $NAMESPACE
data:
  FOLD_INTERNER_BLOB_ENDPOINT: "http://minio-k.default.svc.cluster.local:9000"
  FOLD_INTERNER_BLOB_BUCKET: "internerdata"
  FOLD_INTERNER_FILE_LOCATION: "/tmp/interner"
EOF

echo "DOKS cluster and infrastructure provisioning complete!"
echo "Ready for application deployment. Run 'make k8s-deploy' to deploy application components."