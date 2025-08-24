#!/usr/bin/env bash
set -euo pipefail


# Create the DOKS cluster and wait until it is ready; if it exists, continue
if doctl kubernetes cluster get fold-cluster >/dev/null 2>&1; then
	echo "Cluster 'fold-cluster' already exists; skipping create"
else
	doctl kubernetes cluster create fold-cluster --size s-8vcpu-16gb-intel --count 1 --wait
fi

echo "Configuring DigitalOcean Container Registry access for cluster 'fold-cluster'..."

# Ensure required tools are present
if ! command -v doctl >/dev/null 2>&1; then
	echo "ERROR: 'doctl' is required and not installed. Install doctl and re-run." >&2
	exit 1
fi
if ! command -v yq >/dev/null 2>&1; then
	echo "ERROR: 'yq' is required and not installed. Install yq and re-run." >&2
	exit 1
fi

# Ensure kube namespace exists before applying secrets
kubectl create namespace fold --dry-run=client -o yaml | kubectl apply -f -

# Install metrics-server (idempotent). This enables `kubectl top` and k9s CPU/MEM columns.
echo "Ensuring metrics-server is installed in cluster"
kubectl apply -f https://github.com/kubernetes-sigs/metrics-server/releases/latest/download/components.yaml || true
echo "Waiting for metrics-server rollout (short timeout)"
kubectl -n kube-system rollout status deployment metrics-server --timeout=60s || true

# Load canonical passwords so we can create the application secrets early in the
# provision flow. This makes `passwords.sh` the single source of truth for
# credentials that we apply into the cluster.
if [ -f ./passwords.sh ]; then
	# shellcheck disable=SC1091
	source ./passwords.sh
else
	echo "ERROR: passwords.sh not found in working directory; aborting" >&2
	exit 1
fi

# Create fold-secrets from passwords.sh (idempotent)
if [ -n "${POSTGRES_PASSWORD:-}" ]; then
	echo "Creating/updating fold-secrets from passwords.sh"
	# Compose service URLs that other scripts expect
	DATABASE_URL="postgres://fold:${POSTGRES_PASSWORD}@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
	RABBIT_URL="amqp://user:${RABBIT_PASSWORD}@rabbit-k-rabbitmq.default.svc.cluster.local:5672"

	kubectl create secret generic fold-secrets -n fold \
		--from-literal=postgres-password="$POSTGRES_PASSWORD" \
		--from-literal=rabbit-password="$RABBIT_PASSWORD" \
		--from-literal=minio-access-key="${MINIO_ROOT_USER:-minioadmin}" \
		--from-literal=minio-secret-key="${MINIO_ROOT_PASSWORD:-minioadmin}" \
		--from-literal=FOLD_AMQP_URL="$RABBIT_URL" \
		--from-literal=FOLD_PG_URL="$DATABASE_URL" \
		--dry-run=client -o yaml | kubectl apply -f -
else
	echo "POSTGRES_PASSWORD not set; skipping creation of fold-secrets"
fi

# Generate the DOCR secret manifest (this must succeed)
doctl registry kubernetes-manifest fold > /tmp/docr-secret.yaml
echo "Applying DOCR Kubernetes secret manifest (may target kube-system)"
kubectl apply -f /tmp/docr-secret.yaml

echo "Copying Secret resources from manifest into namespace 'fold'"
secrets=$(yq eval '. | select(.kind=="Secret") .metadata.name' /tmp/docr-secret.yaml)
for s in $secrets; do
	echo "Processing secret: $s"
	# Determine source namespace (manifest may set namespace metadata)
	src_ns=$(yq eval '. | select(.kind=="Secret" and .metadata.name == "'"$s"'" ) .metadata.namespace' /tmp/docr-secret.yaml || true)
	src_ns=${src_ns:-kube-system}

	# Export, strip namespace/resourceVersion/uid/creationTimestamp and apply into fold
	kubectl get secret "$s" -n "$src_ns" -o yaml \
		| yq eval 'del(.metadata.namespace, .metadata.resourceVersion, .metadata.uid, .metadata.creationTimestamp, .metadata.managedFields)' - \
		| kubectl apply -n fold -f -
done

echo "Adding/updating helm repo bitnami..."
helm repo add bitnami https://charts.bitnami.com/bitnami 2>/dev/null || true
helm repo update

echo "Installing/upgrading infra charts (postgres, rabbitmq, minio) into 'default' namespace"
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
	--set auth.rootUser="${MINIO_ROOT_USER:-minioadmin}" \
	--set auth.rootPassword="${MINIO_ROOT_PASSWORD:-minioadmin}" \
	--wait --timeout=3m

# Install Jaeger (all-in-one) for tracing so the app's jaeger endpoint is available in-cluster.
echo "Ensuring Jaeger (all-in-one) is installed"
helm repo add jaegertracing https://jaegertracing.github.io/helm-charts 2>/dev/null || true
helm repo update || true
helm upgrade --install jaeger jaegertracing/jaeger \
	--namespace default --create-namespace \
	--set allInOne.enabled=true \
	--set query.service.type=ClusterIP \
	--wait --timeout=2m || true
echo "Waiting for jaeger-query rollout (short timeout)"
kubectl -n default rollout status deployment jaeger-query --timeout=60s || true

# Sync chart-generated secrets into fold namespace so the application can read them
echo "Syncing infra chart secrets into namespace 'fold'"
if kubectl -n default get secret postgres-k-postgresql >/dev/null 2>&1; then
	PG_B64=$(kubectl -n default get secret postgres-k-postgresql -o jsonpath='{.data.postgres-password}') || true
	if [ -n "$PG_B64" ]; then
		kubectl -n fold create secret generic fold-secrets --from-literal=postgres-password="$(echo -n $PG_B64 | base64 --decode)" --dry-run=client -o yaml | kubectl apply -f - || true
		# update FOLD_PG_URL
		PG_PASS=$(echo -n $PG_B64 | base64 --decode)
		PG_URL="postgres://fold:${PG_PASS}@postgres-k-postgresql.default.svc.cluster.local:5432/fold"
		kubectl -n fold patch secret fold-secrets --type=merge -p "{\"data\":{\"FOLD_PG_URL\":\"$(echo -n $PG_URL | base64)\"}}" || true
	fi
fi

if kubectl -n default get secret minio-k >/dev/null 2>&1; then
	MINIO_USER_B64=$(kubectl -n default get secret minio-k -o jsonpath='{.data.root-user}') || true
	MINIO_PASS_B64=$(kubectl -n default get secret minio-k -o jsonpath='{.data.root-password}') || true
	if [ -n "$MINIO_USER_B64" ] && [ -n "$MINIO_PASS_B64" ]; then
		kubectl -n fold patch secret fold-secrets --type=merge -p "{\"data\":{\"minio-access-key\":\"${MINIO_USER_B64}\",\"minio-secret-key\":\"${MINIO_PASS_B64}\"}}" || true
	fi
fi

# Ensure application ConfigMap and interner blob secret keys exist so pods have
# the FOLD_INTERNER_BLOB_* envs they expect. This keeps provision idempotent and
# makes the feed flow "just work" without manual patching.
echo "Ensuring fold-config and FOLD_INTERNER_BLOB_* in fold-secrets"
if kubectl -n default get secret minio-k >/dev/null 2>&1; then
	MINIO_USER_B64=$(kubectl -n default get secret minio-k -o jsonpath='{.data.root-user}' || true)
	MINIO_PASS_B64=$(kubectl -n default get secret minio-k -o jsonpath='{.data.root-password}' || true)
	if [ -n "$MINIO_USER_B64" ] && [ -n "$MINIO_PASS_B64" ]; then
		# Create fold-config with bucket/endpoint (idempotent)
		kubectl -n fold create configmap fold-config \
			--from-literal=FOLD_INTERNER_BLOB_ENDPOINT='http://minio-k.default.svc.cluster.local:9000' \
			--from-literal=FOLD_INTERNER_BLOB_BUCKET='internerdata' \
			--dry-run=client -o yaml | kubectl apply -f -

		# Copy minio creds into fold-secrets as FOLD_INTERNER_BLOB_* (base64 values)
		kubectl -n fold patch secret fold-secrets --type=merge -p "{\"data\":{\"FOLD_INTERNER_BLOB_ACCESS_KEY\":\"${MINIO_USER_B64}\",\"FOLD_INTERNER_BLOB_SECRET_KEY\":\"${MINIO_PASS_B64}\"}}" || true
		echo "fold-config ensured and fold-secrets patched with FOLD_INTERNER_BLOB_*"
	else
		echo "WARNING: minio chart secret found but could not read root-user/root-password; skipping FOLD_INTERNER_BLOB_* patch"
	fi
else
	echo "WARNING: minio-k secret not found in namespace 'default'; skipping fold-config/fold-secrets patch"
fi

# Ensure DB role 'fold' exists and uses the canonical password from passwords.sh
if kubectl -n default get secret postgres-k-postgresql >/dev/null 2>&1; then
	echo "Ensuring Postgres role 'fold' and database exist with configured password"
	SUPER_PASS=$(kubectl -n default get secret postgres-k-postgresql -o jsonpath='{.data.postgres-password}' | base64 --decode)
		if [ -n "$SUPER_PASS" ]; then
			echo "Waiting for postgres pod to be ready for exec"
			# Wait for a postgres pod to exist and be ready
			pod=""
			for i in $(seq 1 30); do
				pod=$(kubectl -n default get pods -l app.kubernetes.io/instance=postgres-k -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
				if [ -n "$pod" ]; then
					if kubectl -n default exec "$pod" -- pg_isready -U postgres >/dev/null 2>&1; then
						echo "Postgres pod $pod is ready"
						break
					fi
				fi
				sleep 5
			done
			if [ -z "$pod" ]; then
				echo "WARNING: could not find a ready postgres pod to exec into; skipping role sync" >&2
			else
					echo "Ensuring role 'fold' password and database via psql on pod $pod"
					# Pipe the DO $$...$$ SQL into psql on the postgres pod to avoid local $$ -> PID expansion
					printf "%s\n" "DO \$\$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='fold') THEN\n    CREATE ROLE fold LOGIN PASSWORD '%s';\n  ELSE\n    ALTER ROLE fold WITH PASSWORD '%s';\n  END IF;\n  IF NOT EXISTS (SELECT FROM pg_database WHERE datname='fold') THEN\n    CREATE DATABASE fold OWNER fold;\n  END IF;\nEND \$\$;" "$POSTGRES_PASSWORD" "$POSTGRES_PASSWORD" \
						| kubectl -n default exec -i "$pod" -- bash -c "PGPASSWORD='$SUPER_PASS' psql -U postgres -d postgres" || true
			fi
		else
			echo "WARNING: could not read postgres superuser password from chart secret; skipping role sync"
		fi
fi