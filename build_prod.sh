#!/usr/bin/env bash
set -euo pipefail


# Source canonical passwords from passwords.sh so build and helm installs read the
# same values as the provision step. If passwords.sh is missing, fail early.
if [ -f ./passwords.sh ]; then
	# shellcheck disable=SC1091
	source ./passwords.sh
else
	echo "ERROR: passwords.sh is missing. Create it or export required variables." >&2
	exit 1
fi

# Configuration -- override by exporting before running
REGISTRY=${REGISTRY:-registry.digitalocean.com/fold}
IMAGE_NAME=${IMAGE_NAME:-fold}
IMAGE_TAG=${IMAGE_TAG:-latest}
FULL_IMAGE="$REGISTRY/$IMAGE_NAME:$IMAGE_TAG"

HELM_REPO=bitnami
kubectl_opts=${KUBECTL_OPTS:-}

echo "Adding/updating helm repo $HELM_REPO..."
helm repo add $HELM_REPO https://charts.bitnami.com/bitnami
helm repo update

echo "Note: infra (helm charts) are now provisioned in provision_prod.sh; build handles image and app manifests only"


echo "Preparing cluster URLs for builds"
DATABASE_URL="postgres://fold:${POSTGRES_PASSWORD}@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
RABBIT_URL="amqp://user:${RABBIT_PASSWORD}@rabbit-k-rabbitmq.default.svc.cluster.local:5672"

echo "Building docker image: $FULL_IMAGE"

# Prefer docker buildx for reproducible cross-platform images. If MULTIARCH is set to
# "1" build for both linux/amd64 and linux/arm64; otherwise build for linux/amd64
# which matches typical k8s nodes. This will also push the image directly to the
# registry using buildx's --push flag.
BUILD_PLATFORMS=${BUILD_PLATFORMS:-"linux/amd64"}
if [ "${MULTIARCH:-0}" = "1" ]; then
	BUILD_PLATFORMS="linux/amd64,linux/arm64"
fi

# Fail fast if docker or docker buildx aren't available. We want the CI/build
# pipeline to error clearly so the problem can be remediated rather than falling
# back silently to a potentially wrong-arch image.
if ! command -v docker >/dev/null 2>&1; then
	echo "ERROR: 'docker' command not found. Install Docker and retry." >&2
	exit 1
fi

if ! docker buildx version >/dev/null 2>&1; then
	echo "ERROR: 'docker buildx' is not available. Ensure Docker Buildx is installed and enabled." >&2
	echo "On macOS with Docker Desktop you can enable BuildKit/Buildx; or install docker-buildx plugin." >&2
	exit 1
fi

echo "Ensuring a buildx builder is available..."
# Create a named builder if not present and bootstrap (no-op if exists)
if ! docker buildx inspect fold-builder >/dev/null 2>&1; then
	docker buildx create --name fold-builder --use
else
	docker buildx use fold-builder
fi
docker buildx inspect --bootstrap

echo "Building (buildx) for platform(s): $BUILD_PLATFORMS"
docker buildx build \
	--platform "$BUILD_PLATFORMS" \
	--build-arg DATABASE_URL="$DATABASE_URL" \
	--build-arg RABBIT_URL="$RABBIT_URL" \
	-f Dockerfile \
	-t "$FULL_IMAGE" \
	--push \
	.

echo "Creating k8s secrets and config from local passwords (applies/updates)"
kubectl create namespace fold --dry-run=client -o yaml | kubectl apply -f -

# Secrets
# Store both credential pieces and composed service URLs so pods can read them directly
# Use a patchable approach so build can update existing secrets created during
# provision. This keeps passwords.sh as the single source of truth.
kubectl -n fold create secret generic fold-secrets \
	--from-literal=postgres-password="$POSTGRES_PASSWORD" \
	--from-literal=rabbit-password="$RABBIT_PASSWORD" \
	--from-literal=minio-access-key="${MINIO_ROOT_USER:-minioadmin}" \
	--from-literal=minio-secret-key="${MINIO_ROOT_PASSWORD:-minioadmin}" \
	--from-literal=FOLD_AMQP_URL="$RABBIT_URL" \
	--from-literal=FOLD_PG_URL="$DATABASE_URL" \
	--dry-run=client -o yaml | kubectl apply -f -

# ConfigMap for other envs
kubectl create configmap fold-config -n fold --from-literal=FOLD_INTERNER_BLOB_ENDPOINT="http://minio-k.default.svc.cluster.local:9000" --from-literal=FOLD_INTERNER_BLOB_BUCKET="internerdata" --dry-run=client -o yaml | kubectl apply -f -

echo "Rendering k8s manifests from template and applying"
mkdir -p k8s/rendered
IMAGE_TO_SUB="$FULL_IMAGE"

# Require yq to be installed. Fail fast if missing.
if ! command -v yq >/dev/null 2>&1; then
	echo "ERROR: 'yq' is required but not installed. Install it and retry (macOS: 'brew install yq')." >&2
	exit 1
fi

# Render base template to rendered file without image substitution
# Copy per-component templates into rendered dir for image substitution and apply
cp k8s/fold-worker-deploy.yaml.template k8s/rendered/fold-worker-deploy.yaml
cp k8s/fold-feeder-deploy.yaml.template k8s/rendered/fold-feeder-deploy.yaml
cp k8s/fold-follower-deploy.yaml.template k8s/rendered/fold-follower-deploy.yaml
cp k8s/fold-ingestor-deploy.yaml.template k8s/rendered/fold-ingestor-deploy.yaml

# After pushing, try to resolve the linux/amd64 manifest digest from the registry and
# prefer using the digest form (registry/...@sha256:...) for the k8s deployment so
# nodes pull an exact image that matches their architecture. If we can't determine
# the digest, fall back to the tagged image.
IMAGE_TO_SUB="$FULL_IMAGE"
echo "Resolving linux/amd64 digest for $FULL_IMAGE"
raw_manifest=$(docker buildx imagetools inspect "$FULL_IMAGE")
# Parse Name preceding Platform: linux/amd64. Fail if no amd64 manifest is present.
amd64_name=$(echo "$raw_manifest" | awk '/^[[:space:]]*Name:/{name=$2} /Platform:.*linux\/amd64/{if(name){print name; exit}}')
if [ -n "$amd64_name" ]; then
	echo "Found amd64 manifest: $amd64_name"
	IMAGE_TO_SUB="$amd64_name"
else
	echo "ERROR: linux/amd64 manifest not found for $FULL_IMAGE. Aborting." >&2
	exit 1
fi

# Use yq to set all container images on the Deployment document to the chosen image (digest or tag).

# Substitute images and ensure Always pull for each rendered deployment
for f in k8s/rendered/*-deploy.yaml; do
	echo "processing $f"
	yq eval -i ". | select(.kind==\"Deployment\") .spec.template.spec.containers[].image = \"${IMAGE_TO_SUB}\"" "$f"
	yq eval -i ". | select(.kind==\"Deployment\") .spec.template.spec.containers[].imagePullPolicy = \"Always\"" "$f"
	kubectl apply -n fold -f "$f" --dry-run=client
	kubectl apply -n fold -f "$f"
done

# Restart and wait for each deployment rollout
for dep in fold-worker fold-feeder fold-follower fold-ingestor; do
	echo "restarting deployment/$dep"
	kubectl rollout restart deployment/$dep -n fold
	if ! kubectl rollout status deployment/$dep -n fold --timeout=180s; then
		echo "ERROR: rollout for $dep did not complete within 180s; dumping pods and logs" >&2
		kubectl -n fold get pods -o wide
		for p in $(kubectl -n fold get pods -l app=$dep -o jsonpath='{.items[*].metadata.name}'); do
			echo "=== LOGS $p ==="
			kubectl -n fold logs $p --all-containers --tail=300 || true
		done
		exit 1
	fi
done

echo "All done. Deployed image $FULL_IMAGE to k8s namespace 'fold'."
