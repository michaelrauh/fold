#!/bin/bash

# Check queue depths using rabbitmq management
echo "Checking queue depths..."

# Get rabbitmq credentials from environment or use defaults
RABBITMQ_USER=${FOLD_AMQP_USER:-guest}
RABBITMQ_PASSWORD=${FOLD_AMQP_PASSWORD:-guest}
RABBITMQ_HOST=${FOLD_AMQP_HOST:-localhost}
RABBITMQ_PORT=${FOLD_AMQP_PORT:-15672}

# Check workq depth
workq_depth=$(curl -s -u "${RABBITMQ_USER}:${RABBITMQ_PASSWORD}" \
    "http://${RABBITMQ_HOST}:${RABBITMQ_PORT}/api/queues/%2F/workq" | \
    grep -o '"messages":[0-9]*' | cut -d: -f2)

# Check dbq depth  
dbq_depth=$(curl -s -u "${RABBITMQ_USER}:${RABBITMQ_PASSWORD}" \
    "http://${RABBITMQ_HOST}:${RABBITMQ_PORT}/api/queues/%2F/dbq" | \
    grep -o '"messages":[0-9]*' | cut -d: -f2)

echo "workq depth: ${workq_depth:-0}"
echo "dbq depth: ${dbq_depth:-0}"