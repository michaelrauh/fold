#!/usr/bin/env bash
set -euo pipefail

# Simple monitoring like the make targets
NAMESPACE=${NAMESPACE:-fold}

echo "Monitoring fold application..."

# Get pod status
kubectl get pods -n $NAMESPACE

# Get queue and database depths using any available pod
POD=$(kubectl get pod -l app=fold-feeder -n $NAMESPACE -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
if [ -n "$POD" ]; then
    echo -e "\nQueue depths:"
    kubectl exec -n $NAMESPACE "$POD" --env="FOLD_AMQP_HOST=rabbit-k-rabbitmq.default.svc.cluster.local" \
        --env="FOLD_AMQP_PORT=15672" --env="FOLD_AMQP_USER=user" --env="FOLD_AMQP_PASSWORD=foldpass" \
        -- /app/scripts/queue_depths.sh 2>/dev/null || echo "Unable to get queue depths"
    
    echo -e "\nDatabase count:"
    kubectl exec -n $NAMESPACE "$POD" --env="FOLD_POSTGRES_HOST=postgres-k-postgresql.default.svc.cluster.local" \
        --env="FOLD_POSTGRES_PORT=5432" --env="FOLD_POSTGRES_DB=fold" \
        --env="FOLD_POSTGRES_USER=fold" --env="FOLD_POSTGRES_PASSWORD=foldpass" \
        -- /app/scripts/database_ops.sh size 2>/dev/null || echo "Unable to get database count"
else
    echo "No feeder pod found - ensure application is deployed"
fi