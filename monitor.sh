#!/usr/bin/env bash
set -euo pipefail

# Simple monitoring like the make targets
NAMESPACE=${NAMESPACE:-fold}

echo "Monitoring fold application..."

# Get pod status
kubectl get pods -n $NAMESPACE

# Get queue and database depths using ingestor pod
POD=$(kubectl get pod -l app=fold-ingestor -n $NAMESPACE -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
if [ -n "$POD" ]; then
    echo -e "\nQueue depths:"
    kubectl exec -n $NAMESPACE "$POD" -- /app/ingestor queues 2>/dev/null || echo "Unable to get queue depths"
    
    echo -e "\nDatabase count:"
    kubectl exec -n $NAMESPACE "$POD" -- /app/ingestor database 2>/dev/null || echo "Unable to get database count"
else
    echo "No ingestor pod found - ensure application is deployed"
fi