# Kubernetes Deployment

This directory contains simplified Kubernetes deployment manifests for the fold application.

## Complete Workflow

The fold application can be deployed to Kubernetes using a complete workflow:

### 1. Provision Infrastructure
Set up required infrastructure dependencies (PostgreSQL, RabbitMQ, MinIO):
```bash
make k8s-provision
```

### 2. Build Application
Build and push the Docker image:
```bash
make k8s-build
```

### 3. Deploy Application
Deploy the fold application to Kubernetes:
```bash
make k8s-deploy
```

### 4. Feed Data
Feed initial data to the system:
```bash
make k8s-feed
```

### 5. Monitor
Monitor the running application:
```bash
make k8s-monitor
```

## Quick Commands

- **Check deployment status**: `make k8s-status`
- **Scale workers**: `REPLICAS=5 make k8s-scale`
- **Clean up**: `make k8s-clean`

## Manual Deployment

If you prefer manual control:

1. Provision infrastructure:
   ```bash
   ./provision.sh
   ```

2. Build the Docker image:
   ```bash
   ./build.sh
   ```

3. Deploy to Kubernetes:
   ```bash
   ./deploy.sh
   ```

4. Feed data:
   ```bash
   ./feed.sh
   ```

5. Monitor:
   ```bash
   ./monitor.sh
   ```

## Infrastructure Components

The provision script creates:

- **PostgreSQL**: Database for storing ortho data
- **RabbitMQ**: Message queue for work distribution
- **MinIO**: S3-compatible object storage for blob data
- **Secrets**: Contains database credentials, queue URLs, and storage keys
- **ConfigMaps**: Contains storage endpoints and configuration

## Application Components

- **fold-worker**: Processes work items from the queue
- **fold-feeder**: Feeds the system with new work
- **fold-follower**: Follows and processes updates
- **fold-ingestor**: Ingests data and provides status

All components are deployed as separate Kubernetes Deployments in the `fold` namespace.