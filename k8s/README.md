# Kubernetes Deployment

This directory contains simplified Kubernetes deployment manifests for the fold application.

## Quick Start

1. Build and deploy to Kubernetes:
   ```bash
   make k8s-deploy
   ```

2. Check deployment status:
   ```bash
   make k8s-status
   ```

3. Scale workers:
   ```bash
   REPLICAS=5 make k8s-scale
   ```

4. Clean up:
   ```bash
   make k8s-clean
   ```

## Manual Deployment

If you prefer manual control:

1. Build the Docker image:
   ```bash
   docker build -t fold:latest .
   docker tag fold:latest your-registry/fold:latest
   docker push your-registry/fold:latest
   ```

2. Update the image in the deployment files:
   ```bash
   sed -i 's|image: fold:latest|image: your-registry/fold:latest|g' k8s/*-deployment.yaml
   ```

3. Apply the manifests:
   ```bash
   kubectl apply -f k8s/
   ```

## Configuration

The deployment expects the following secrets and config maps to be created in the cluster:

- `fold-secrets`: Contains database credentials, queue URLs, and blob storage keys
- `fold-config`: Contains blob storage endpoint and bucket configuration

You'll need to create these manually or use a tool like Helm to manage your infrastructure dependencies (PostgreSQL, RabbitMQ, MinIO).

## Components

- **fold-worker**: Processes work items from the queue
- **fold-feeder**: Feeds the system with new work
- **fold-follower**: Follows and processes updates
- **fold-ingestor**: Ingests data and provides status (runs in daemon mode)

All components are deployed as separate Kubernetes Deployments in the `fold` namespace.