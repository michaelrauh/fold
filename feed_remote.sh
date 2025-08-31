#!/bin/bash
set -e

# Remote feed script that works against the Kubernetes cluster
# This connects to remote services directly instead of running scripts in pods

NAMESPACE=${NAMESPACE:-fold}

echo "Running feed sequence against remote Kubernetes cluster..."

# Check if e.txt exists locally
if [[ ! -f "e.txt" ]]; then
    echo "Error: e.txt not found locally. Please ensure e.txt exists in the current directory."
    exit 1
fi

# Get service endpoints
echo "Getting service endpoints..."
MINIO_IP=$(kubectl get service minio-k -n $NAMESPACE -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || echo "")
POSTGRES_IP=$(kubectl get service postgres-k-postgresql -n $NAMESPACE -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || echo "")
RABBITMQ_IP=$(kubectl get service rabbit-k-rabbitmq -n $NAMESPACE -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || echo "")

# If no external IPs, use port-forwarding
if [[ -z "$MINIO_IP" || -z "$POSTGRES_IP" || -z "$RABBITMQ_IP" ]]; then
    echo "Setting up port-forwards to services..."
    kubectl port-forward service/minio-k 9000:9000 -n $NAMESPACE &
    MINIO_PF_PID=$!
    kubectl port-forward service/postgres-k-postgresql 5432:5432 -n $NAMESPACE &
    POSTGRES_PF_PID=$!
    kubectl port-forward service/rabbit-k-rabbitmq 5672:5672 -n $NAMESPACE &
    RABBITMQ_PF_PID=$!
    sleep 5
    
    MINIO_ENDPOINT="http://localhost:9000"
    POSTGRES_URL="postgresql://fold:foldpass@localhost:5432/fold"
    AMQP_URL="amqp://user:foldpass@localhost:5672/"
else
    MINIO_ENDPOINT="http://$MINIO_IP:9000"
    POSTGRES_URL="postgresql://fold:foldpass@$POSTGRES_IP:5432/fold"
    AMQP_URL="amqp://user:foldpass@$RABBITMQ_IP:5672/"
fi

echo "Using endpoints:"
echo "  MinIO: $MINIO_ENDPOINT"
echo "  PostgreSQL: $POSTGRES_URL"
echo "  RabbitMQ: $AMQP_URL"

# Configure MinIO client
mc alias set k8s "$MINIO_ENDPOINT" minioadmin minioadmin

echo "Uploading e.txt to remote S3..."
mc cp e.txt k8s/internerdata/e.txt

echo "Splitting e.txt..."
# Simple splitting - create parts based on CHAPTER delimiter
awk '/CHAPTER/{close(fname); fname="e.txt-part-"++i} fname{print > fname}' e.txt
part_files=$(ls e.txt-part-* 2>/dev/null || echo "")

if [[ -n "$part_files" ]]; then
    echo "Uploading split parts..."
    for part in $part_files; do
        mc cp "$part" "k8s/internerdata/$part"
    done
    
    echo "Feeding split parts using local feed_util..."
    # Set environment variables for remote services
    export FOLD_INTERNER_BLOB_ENDPOINT="$MINIO_ENDPOINT"
    export FOLD_INTERNER_BLOB_BUCKET="internerdata"
    export FOLD_INTERNER_BLOB_ACCESS_KEY="minioadmin"
    export FOLD_INTERNER_BLOB_SECRET_KEY="minioadmin"
    export FOLD_AMQP_URL="$AMQP_URL"
    export FOLD_PG_URL="$POSTGRES_URL"
    
    # Use local feed_util to connect to remote services
    for part in $part_files; do
        echo "Feeding $part..."
        if [[ -x "./target/release/feed_util" ]]; then
            cat "$part" | ./target/release/feed_util
        elif [[ -x "/app/feed_util" ]]; then
            cat "$part" | /app/feed_util
        else
            echo "Error: feed_util not found. Please build the project first."
            exit 1
        fi
        rm "$part"  # Clean up local part
        sleep 1
    done
else
    echo "No split parts found, feeding entire file..."
    export FOLD_INTERNER_BLOB_ENDPOINT="$MINIO_ENDPOINT"
    export FOLD_INTERNER_BLOB_BUCKET="internerdata"
    export FOLD_INTERNER_BLOB_ACCESS_KEY="minioadmin"
    export FOLD_INTERNER_BLOB_SECRET_KEY="minioadmin"
    export FOLD_AMQP_URL="$AMQP_URL"
    export FOLD_PG_URL="$POSTGRES_URL"
    
    if [[ -x "./target/release/feed_util" ]]; then
        cat e.txt | ./target/release/feed_util
    elif [[ -x "/app/feed_util" ]]; then
        cat e.txt | /app/feed_util
    else
        echo "Error: feed_util not found. Please build the project first."
        exit 1
    fi
fi

# Clean up port-forwards if used
for pid in $MINIO_PF_PID $POSTGRES_PF_PID $RABBITMQ_PF_PID; do
    if [[ -n "$pid" ]]; then
        kill $pid 2>/dev/null || true
    fi
done

echo "Remote feed completed. Monitor progress with:"
echo "  kubectl logs -f deployment/fold-feeder -n $NAMESPACE"
        --env="FOLD_AMQP_USER=user" \
        --env="FOLD_AMQP_PASSWORD=foldpass" \
        -- /app/scripts/"$script_name" "$@"
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
run_script s3_ops.sh split s3://internerdata/e.txt CHAPTER

echo "Listing S3 objects after split..."
run_mc "mc ls localminio/internerdata"

echo "Cleaning small files (size < 100)..."
run_script s3_ops.sh clean-small 100

echo "Listing S3 objects after cleanup..."
run_mc "mc ls localminio/internerdata"

echo "Feeding data from e.txt-part-56..."
run_script interner_ops.sh feed-s3 s3://internerdata/e.txt-part-56 || echo "Part 56 may not exist, continuing..."

echo "Checking database count..."
run_script database_ops.sh size

echo "Checking queue count..."
run_script queue_depths.sh

echo "Checking interner versions..."
run_script interner_ops.sh versions

echo "Waiting 10 seconds..."
sleep 10

echo "Feeding data from e.txt-part-55..."
run_script interner_ops.sh feed-s3 s3://internerdata/e.txt-part-55 || echo "Part 55 may not exist, continuing..."

echo "Checking database count..."
run_script database_ops.sh size

echo "Checking queue count..."
run_script queue_depths.sh

echo "Scaling workers to 50 replicas..."
kubectl scale deployment fold-worker --replicas=50 -n $NAMESPACE

echo "Waiting 10 seconds..."
sleep 10

echo "Checking database count..."
run_script database_ops.sh size

echo "Checking queue count..."
run_script queue_depths.sh

echo "Printing optimal result..."
run_script database_ops.sh optimal

echo "Showing recent logs..."
kubectl logs -n $NAMESPACE deployment/fold-worker --tail=20
kubectl logs -n $NAMESPACE deployment/fold-feeder --tail=20
kubectl logs -n $NAMESPACE deployment/fold-follower --tail=20

echo "Remote feed sequence completed!"