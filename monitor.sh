#!/usr/bin/env bash
set -euo pipefail

# Monitor fold application running in Kubernetes

NAMESPACE=${NAMESPACE:-fold}

echo "Monitoring fold application in Kubernetes..."

# Function to show a separator line
separator() {
    echo "=================================="
}

# Show deployment status
echo "Deployment Status:"
separator
kubectl get deployments -n $NAMESPACE
echo

# Show pod status
echo "Pod Status:"
separator
kubectl get pods -n $NAMESPACE
echo

# Show service status
echo "Service Status:"
separator
kubectl get services -n $NAMESPACE
echo

# Show resource usage
echo "Resource Usage:"
separator
kubectl top pods -n $NAMESPACE 2>/dev/null || echo "Metrics server not available - install it to see resource usage"
echo

# Check application logs
echo "Recent Application Logs:"
separator

# Show logs from each component
components=("fold-worker" "fold-feeder" "fold-follower" "fold-ingestor")
for component in "${components[@]}"; do
    echo "--- $component logs (last 10 lines) ---"
    kubectl logs -l app=$component -n $NAMESPACE --tail=10 2>/dev/null || echo "No logs available for $component"
    echo
done

# Show infrastructure logs
echo "--- Infrastructure Status ---"
echo "PostgreSQL:"
kubectl logs -l app=postgres -n $NAMESPACE --tail=5 2>/dev/null || echo "PostgreSQL logs not available"
echo

echo "RabbitMQ:"
kubectl logs -l app=rabbitmq -n $NAMESPACE --tail=5 2>/dev/null || echo "RabbitMQ logs not available"
echo

echo "MinIO:"
kubectl logs -l app=minio -n $NAMESPACE --tail=5 2>/dev/null || echo "MinIO logs not available"
echo

# Show application-specific metrics if available
echo "Application Metrics:"
separator
echo "Attempting to get queue depths and database status..."

# Try to get queue depths
echo "Queue Depths:"
kubectl exec -n $NAMESPACE deployment/fold-ingestor -- /app/ingestor queues 2>/dev/null || echo "Unable to get queue depths"

# Try to get database status
echo "Database Status:"
kubectl exec -n $NAMESPACE deployment/fold-ingestor -- /app/ingestor database 2>/dev/null || echo "Unable to get database status"

echo
echo "Monitoring complete. For continuous monitoring, run:"
echo "kubectl logs -f -l app=fold-worker -n $NAMESPACE"
echo "kubectl logs -f -l app=fold-feeder -n $NAMESPACE"