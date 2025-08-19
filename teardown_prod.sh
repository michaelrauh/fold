#!/usr/bin/env bash
set -euo pipefail

NAMESPACE=${NAMESPACE:-fold}
RELEASES=(postgres-k rabbit-k minio-k)

echo "Teardown: uninstall helm releases (${RELEASES[*]}) and delete namespace '$NAMESPACE'"

for r in "${RELEASES[@]}"; do
  if helm status "$r" >/dev/null 2>&1; then
    echo "Uninstalling helm release $r..."
    helm uninstall "$r" || true
  else
    echo "Helm release $r not found, skipping."
  fi
done

echo "Deleting k8s namespace '$NAMESPACE' (and all contained resources)"
kubectl delete namespace "$NAMESPACE" --ignore-not-found

# If namespace still exists (delete may be async/failing), attempt to remove docker registry secrets inside it
if kubectl get namespace "$NAMESPACE" >/dev/null 2>&1; then
  echo "Namespace $NAMESPACE still exists; removing dockerconfigjson-type secrets inside it..."
  # list secrets of type dockerconfigjson and delete them
  secrets=$(kubectl -n "$NAMESPACE" get secrets -o jsonpath='{range .items[?(@.type=="kubernetes.io/dockerconfigjson")]}{.metadata.name} {end}' || true)
  if [ -n "$secrets" ]; then
    for s in $secrets; do
      echo "Deleting secret $s in $NAMESPACE"
      kubectl delete secret "$s" -n "$NAMESPACE" --ignore-not-found || true
    done
  else
    echo "No dockerconfigjson-type secrets found in $NAMESPACE."
  fi
fi

# Remove any rendered manifests to allow clean re-render
if [ -d k8s/rendered ]; then
  echo "Removing k8s/rendered directory"
  rm -rf k8s/rendered || true
fi

echo "Teardown complete."

# Aggressive cleanup for test environments: remove PVCs and PVs created by the chart releases
echo "Cleaning up PVCs/PVs for releases in namespace 'default'..."
for r in "${RELEASES[@]}"; do
  echo "Deleting PVCs with label app.kubernetes.io/instance=$r in namespace default"
  kubectl -n default delete pvc -l app.kubernetes.io/instance="$r" --ignore-not-found || true
done

# Delete any remaining PVCs in default (test env - aggressive)
echo "Deleting any remaining PVCs in 'default' namespace..."
kubectl -n default get pvc -o name | xargs -r kubectl -n default delete --ignore-not-found || true

# Delete PVs whose claimRef referenced the default namespace (clean up underlying storage)
pv_list=$(kubectl get pv -o jsonpath='{range .items[?(@.spec.claimRef.namespace=="default")]}{.metadata.name} {end}' || true)
if [ -n "$pv_list" ]; then
  echo "Deleting PVs claimed by default namespace: $pv_list"
  for pv in $pv_list; do
    kubectl delete pv "$pv" --ignore-not-found || true
  done
else
  echo "No PVs claimed by default namespace found."
fi

echo "Final namespace/pvc status (brief):"
kubectl get ns --ignore-not-found
kubectl -n default get pvc --ignore-not-found || true
kubectl get pv --ignore-not-found || true
