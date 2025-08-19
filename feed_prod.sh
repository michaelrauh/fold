
#!/usr/bin/env bash
set -euo pipefail

# feed_prod.sh - fully automated test flow for provisioning + feeding
# Usage: make feed-prod

NAMESPACE=${NAMESPACE:-fold}
MINIO_NS=${MINIO_NS:-default}
FILE=${FILE:-e.txt}
DELIM=${DELIM:-CHAPTER}
SPLIT_JOB=${SPLIT_JOB:-fold-split-job}
S3_ALIAS=${S3_ALIAS:-prodminio}
SIZE=${SIZE:-100}

die(){ echo "$@" >&2; exit 1; }

# 1) ensure repo file exists
[[ -f "$FILE" ]] || die "file '$FILE' not found"

# 2) provision cluster infra if necessary
if ! kubectl get ns "$NAMESPACE" >/dev/null 2>&1; then
  echo "-> namespace $NAMESPACE not found: running ./provision_prod.sh"
  ./provision_prod.sh
  echo "-> provision step completed"
fi

# 3) ensure fold-secrets exist; create them from passwords.sh if missing
if ! kubectl -n "$NAMESPACE" get secret fold-secrets >/dev/null 2>&1; then
  if [[ -f passwords.sh ]]; then
    echo "-> creating fold-secrets from passwords.sh"
    # shellcheck disable=SC1090
    source passwords.sh
    kubectl -n "$NAMESPACE" create secret generic fold-secrets \
      --from-literal=MINIO_ROOT_USER="$MINIO_ROOT_USER" \
      --from-literal=MINIO_ROOT_PASSWORD="$MINIO_ROOT_PASSWORD" \
      --from-literal=POSTGRES_PASSWORD="$POSTGRES_PASSWORD" \
      --from-literal=RABBIT_PASSWORD="$RABBIT_PASSWORD" || true
  else
    die "fold-secrets missing and passwords.sh not found"
  fi
fi

# 4) port-forward MinIO and wait
echo "-> starting port-forward to MinIO svc/minio-k in namespace $MINIO_NS"
kubectl -n "$MINIO_NS" port-forward svc/minio-k 9000:9000 >/dev/null 2>&1 &
PF_PID=$!
trap 'kill "$PF_PID" >/dev/null 2>&1 || true' EXIT

for i in {1..20}; do
  if curl -sS http://localhost:9000/ >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
if ! curl -sS http://localhost:9000/ >/dev/null 2>&1; then
  die "timed out waiting for localhost:9000 to respond"
fi

# 5) configure mc alias using secrets from cluster
# Read credentials: prefer MINIO_ROOT_USER/PASSWORD, fall back to minio-access-key/secret
MINIO_ROOT_USER=""
MINIO_ROOT_PASSWORD=""
if kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.MINIO_ROOT_USER}' >/dev/null 2>&1; then
  MINIO_ROOT_USER=$(kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.MINIO_ROOT_USER}' | base64 -d)
fi
if kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.MINIO_ROOT_PASSWORD}' >/dev/null 2>&1; then
  MINIO_ROOT_PASSWORD=$(kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.MINIO_ROOT_PASSWORD}' | base64 -d)
fi
if [[ -z "${MINIO_ROOT_USER:-}" || -z "${MINIO_ROOT_PASSWORD:-}" ]]; then
  # try minio-access-key / minio-secret-key (Bitnami helm names)
  if kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.minio-access-key}' >/dev/null 2>&1; then
    MINIO_ROOT_USER=$(kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.minio-access-key}' | base64 -d)
  fi
  if kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.minio-secret-key}' >/dev/null 2>&1; then
    MINIO_ROOT_PASSWORD=$(kubectl -n "$NAMESPACE" get secret fold-secrets -o jsonpath='{.data.minio-secret-key}' | base64 -d)
  fi
fi
if [[ -z "${MINIO_ROOT_USER:-}" || -z "${MINIO_ROOT_PASSWORD:-}" ]]; then
  die "unable to locate MinIO credentials in fold-secrets"
fi
mc alias set "$S3_ALIAS" http://localhost:9000 "$MINIO_ROOT_USER" "$MINIO_ROOT_PASSWORD" >/dev/null

# 6) ensure bucket exists
mc mb "$S3_ALIAS"/internerdata || true

echo "-> upload $FILE to s3://internerdata/"
mc cp "$FILE" "$S3_ALIAS"/internerdata/ || die "upload failed"

# 7) run in-cluster split job using default image
FULL_IMAGE=${FULL_IMAGE:-registry.digitalocean.com/fold/fold:latest}
echo "-> using image: $FULL_IMAGE for split (creating pod ${SPLIT_JOB}-pod)"
# create a one-off pod to run the split (some clusters reject Job YAML via stdin); simpler and deterministic
kubectl -n "$NAMESPACE" delete pod "${SPLIT_JOB}-pod" --ignore-not-found || true
cat <<EOF | kubectl -n "$NAMESPACE" apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: ${SPLIT_JOB}-pod
spec:
  restartPolicy: Never
  containers:
  - name: split
    image: ${FULL_IMAGE}
    command: ["/app/ingestor","ingest-s3-split","s3://internerdata/${FILE}","${DELIM}"]
    envFrom:
    - secretRef:
        name: fold-secrets
    - configMapRef:
        name: fold-config
EOF

# Wait for pod to reach Succeeded/Failed
TIMEOUT=${SPLIT_TIMEOUT:-300}
SECS_WAITED=0
while true; do
  PHASE=$(kubectl -n "$NAMESPACE" get pod "${SPLIT_JOB}-pod" -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
  if [[ "$PHASE" == "Succeeded" ]]; then
    echo "-> split pod succeeded"
    break
  fi
  if [[ "$PHASE" == "Failed" ]]; then
    echo "-> split pod failed"
    break
  fi
  if [[ -z "$PHASE" ]]; then
    echo "-> pod not yet created"
  else
    echo "-> pod phase=$PHASE"
  fi
  sleep 2
  SECS_WAITED=$((SECS_WAITED+2))
  if [[ $SECS_WAITED -ge $TIMEOUT ]]; then
    kubectl -n "$NAMESPACE" get pod "${SPLIT_JOB}-pod" -o wide || true
    die "timed out waiting for split pod"
  fi
done

echo "-> split pod logs"
kubectl -n "$NAMESPACE" logs "${SPLIT_JOB}-pod" --all-containers || true
kubectl -n "$NAMESPACE" delete pod "${SPLIT_JOB}-pod" --ignore-not-found || true

# 8) pick an ingestor pod and run feeding steps
POD=$(kubectl -n "$NAMESPACE" get pod -l app=fold-ingestor -o jsonpath='{.items[0].metadata.name}')
[[ -n "$POD" ]] || die "fold-ingestor pod not found in namespace $NAMESPACE"

kubectl -n "$NAMESPACE" exec "$POD" -c ingestor -- /app/ingestor clean-s3-small "$SIZE"

echo "-> feeding parts"
PARTS=$(mc ls --recursive "$S3_ALIAS"/internerdata | awk '{print $NF}' | grep "^${FILE}-part-" | sort || true)
for p in $PARTS; do
  echo "-> feeding $p"
  kubectl -n "$NAMESPACE" exec "$POD" -c ingestor -- /app/ingestor feed-s3 s3://internerdata/"$p"
done

kubectl -n "$NAMESPACE" exec "$POD" -c ingestor -- /app/ingestor database
kubectl -n "$NAMESPACE" exec "$POD" -c ingestor -- /app/ingestor queues
kubectl -n "$NAMESPACE" exec "$POD" -c ingestor -- /app/ingestor interner-versions || true

# 9) scale workers to 50 for test load
kubectl -n "$NAMESPACE" scale deployment fold-worker --replicas=50 || true

echo "-> tailing logs (ctrl-c to stop)"
kubectl -n "$NAMESPACE" logs -l app=fold-ingestor -f

