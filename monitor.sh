#!/usr/bin/env bash
set -euo pipefail

# Monitor fold application running in Kubernetes
# Shows queue and database depths similar to make targets

NAMESPACE=${NAMESPACE:-fold}

echo "Monitoring fold application in Kubernetes..."

# Check if we have any running pods first
echo "Checking pod status:"
kubectl get pods -n $NAMESPACE

echo ""
echo "Getting queue depths:"
# Find an ingestor pod and run the queues command
POD=$(kubectl get pod -l app=fold-ingestor -n $NAMESPACE -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
if [ -n "$POD" ]; then
    kubectl exec -n $NAMESPACE "$POD" -- /app/ingestor queues 2>/dev/null || echo "Unable to get queue depths"
else
    echo "No ingestor pod found - ensure application is deployed"
fi

echo ""
echo "Getting database count:"
# Use the same pod to get database status
if [ -n "$POD" ]; then
    kubectl exec -n $NAMESPACE "$POD" -- /app/ingestor database 2>/dev/null || echo "Unable to get database count"
else
    echo "No ingestor pod found - ensure application is deployed"
fi

echo ""
echo "For continuous monitoring, you can run:"
echo "  watch kubectl exec -n $NAMESPACE deployment/fold-ingestor -- /app/ingestor queues"
echo "  kubectl logs -f -l app=fold-worker -n $NAMESPACE"