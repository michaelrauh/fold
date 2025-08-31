#!/bin/bash
set -e

# Remote feed script that works against the Kubernetes cluster
# Uses kubectl to run ingestor commands inside the cluster

NAMESPACE=${NAMESPACE:-fold}

echo "Running feed sequence against remote Kubernetes cluster..."

# Helper function to run ingestor commands in the cluster
run_ingestor() {
    kubectl run ingestor-cmd-$(date +%s) --rm -i --restart=Never \
        --image=$(kubectl get deployment fold-ingestor -n $NAMESPACE -o jsonpath='{.spec.template.spec.containers[0].image}') \
        --namespace=$NAMESPACE \
        --env="FOLD_AMQP_URL=amqp://user:foldpass@rabbit-k-rabbitmq.default.svc.cluster.local:5672/" \
        --env="FOLD_PG_URL=postgresql://fold:foldpass@postgres-k-postgresql.default.svc.cluster.local:5432/fold" \
        --env="FOLD_INTERNER_BLOB_ENDPOINT=http://minio-k.default.svc.cluster.local:9000" \
        --env="FOLD_INTERNER_BLOB_BUCKET=internerdata" \
        --env="FOLD_INTERNER_BLOB_ACCESS_KEY=minioadmin" \
        --env="FOLD_INTERNER_BLOB_SECRET_KEY=minioadmin" \
        -- /app/ingestor "$@"
}

# Helper function to run mc commands for MinIO operations
run_mc() {
    kubectl run mc-cmd-$(date +%s) --rm -i --restart=Never \
        --image=minio/mc \
        --namespace=default \
        -- sh -c "
        mc alias set localminio http://minio-k.default.svc.cluster.local:9000 minioadmin minioadmin
        $*"
}

echo "Listing S3 objects..."
run_mc "mc ls localminio/internerdata"

echo "Uploading local file e.txt..."
# Check if e.txt exists locally
if [[ ! -f "e.txt" ]]; then
    echo "Error: e.txt file not found. Please create a local e.txt file with CHAPTER delimiters."
    exit 1
fi

# Upload the real local file via a temporary pod
cat e.txt | kubectl run file-upload-$(date +%s) --rm -i --restart=Never \
    --image=minio/mc \
    --namespace=default \
    -- sh -c "
    cat > /tmp/e.txt
    mc alias set localminio http://minio-k.default.svc.cluster.local:9000 minioadmin minioadmin
    mc cp /tmp/e.txt localminio/internerdata/e.txt"

echo "Splitting file by CHAPTER delimiter..."
run_ingestor ingest-s3-split s3://internerdata/e.txt CHAPTER

echo "Listing S3 objects after split..."
run_mc "mc ls localminio/internerdata"

echo "Cleaning small files (size < 100)..."
run_ingestor clean-s3-small 100

echo "Listing S3 objects after cleanup..."
run_mc "mc ls localminio/internerdata"

echo "Feeding data from e.txt-part-56..."
run_ingestor feed-s3 s3://internerdata/e.txt-part-56 || echo "Part 56 may not exist, continuing..."

echo "Checking database count..."
run_ingestor database

echo "Checking queue count..."
run_ingestor queues

echo "Checking interner versions..."
run_ingestor interner-versions

echo "Waiting 10 seconds..."
sleep 10

echo "Feeding data from e.txt-part-55..."
run_ingestor feed-s3 s3://internerdata/e.txt-part-55 || echo "Part 55 may not exist, continuing..."

echo "Checking database count..."
run_ingestor database

echo "Checking queue count..."
run_ingestor queues

echo "Scaling workers to 50 replicas..."
kubectl scale deployment fold-worker --replicas=50 -n $NAMESPACE

echo "Waiting 10 seconds..."
sleep 10

echo "Checking database count..."
run_ingestor database

echo "Checking queue count..."
run_ingestor queues

echo "Printing optimal result..."
run_ingestor print-optimal

echo "Showing recent logs..."
kubectl logs -n $NAMESPACE deployment/fold-worker --tail=20
kubectl logs -n $NAMESPACE deployment/fold-feeder --tail=20
kubectl logs -n $NAMESPACE deployment/fold-follower --tail=20

echo "Remote feed sequence completed!"