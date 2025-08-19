start:
	${MAKE} reset
	sleep 15
	./feed.sh

# Directory used by the interner for temporary blob files. Set this to a
# path inside the repo or an explicit directory. Defaults to a safe local
# tmp path to avoid accidental removal of system files when running `make clean`.

build:
	docker build -t fold:latest -f Dockerfile .

up: build
	docker-compose up --build -d

down:
	docker-compose down -v

reset:
	$(MAKE) down
	$(MAKE) up

test:
	cargo test


# `clean` target intentionally removed per user request (do not run destructive clean here).

setup-s3:
	mc alias set localminio http://localhost:9000 minioadmin minioadmin || true

list-s3:
	mc ls localminio/internerdata

split:
	# Usage: make split FILE=yourfile.txt DELIM="\n"
	docker compose run --rm ingestor /app/ingestor ingest-s3-split s3://internerdata/$(FILE) $(DELIM)

queue-count:
	docker compose run --rm ingestor /app/ingestor queues

db-count:
	docker compose run --rm ingestor /app/ingestor database

optimal:
	docker compose run --rm ingestor /app/ingestor print-optimal

logs:
	docker-compose logs -f 

services:
	docker compose config --services

show-s3:
	# Usage: make show-s3 FILE=yourfile.txt
	mc cat localminio/internerdata/$(FILE)

delete-s3:
	# Usage: make delete-s3 FILE=yourfile.txt
	mc rm localminio/internerdata/$(FILE)

put-s3:
	# Usage: make put-s3 FILE=yourfile.txt
	mc cp $(FILE) localminio/internerdata/$(FILE)

clean-s3-small:
	# Usage: make clean-s3-small SIZE=1000
	docker compose run --rm ingestor /app/ingestor clean-s3-small $(SIZE)

feed-s3:
	docker compose run --rm ingestor /app/ingestor feed-s3 s3://internerdata/$(FILE)

help-ingestor:
	docker compose run --rm ingestor /app/ingestor --help

interner-versions:
	docker compose run --rm ingestor /app/ingestor interner-versions

version-counts:
	docker compose run --rm ingestor /app/ingestor version-counts

scale-worker:
	docker compose up --scale fold_worker=$(REPLICAS) -d

scale-feeder:
	docker compose up --scale feeder=$(REPLICAS) -d

stats:
	docker stats

scale-status:
	@echo "fold_worker: $$(docker ps --filter 'name=fold_worker' --format '{{.Names}}' | wc -l)"
	@echo "feeder: $$(docker ps --filter 'name=feeder' --format '{{.Names}}' | wc -l)"
	@echo "follower: $$(docker ps --filter 'name=follower' --format '{{.Names}}' | wc -l)"

# Scale production worker deployment (Kubernetes)
# Usage: REPLICAS=3 make scale-prod-worker
scale-prod-worker:
	@echo "Attempting to scale worker in namespace $(NAMESPACE) to $(REPLICAS) replicas..."
	@sh -c '\
if kubectl -n $(NAMESPACE) get deployment fold-worker >/dev/null 2>&1; then \
	echo "Found deployment 'fold-worker' — scaling it to $(REPLICAS)..."; \
	kubectl -n $(NAMESPACE) scale deployment fold-worker --replicas=$(REPLICAS); \
	kubectl -n $(NAMESPACE) get deployment fold-worker -o wide || true; \
else \
	echo "Deployment 'fold-worker' not found; nothing to scale."; \
fi'

feeder-stats:
	# Follow feeder container logs and show only stats lines
	docker compose logs -f feeder 2>&1 | grep -F '[feeder][stats]'

feeder-stats-once:
	# Show last 200 feeder stats lines
	docker compose logs --tail=200 feeder 2>&1 | grep -F '[feeder][stats]'

feeder-cache:
	# Follow feeder container logs and show only cache lines
	docker compose logs -f feeder 2>&1 | grep -F '[feeder][cache]'

feeder-cache-once:
	# Show last 200 feeder cache lines
	docker compose logs --tail=200 feeder 2>&1 | grep -F '[feeder][cache]'

follower-stats:
	# Follow follower container logs and show only stats lines
	docker compose logs -f follower 2>&1 | grep -F '[follower][stats]'

follower-stats-once:
	# Show last 200 follower stats lines
	docker compose logs --tail=200 follower 2>&1 | grep -F '[follower][stats]'

follower-diff:
	# Follow follower logs and show only diff production lines
	docker compose logs -f follower 2>&1 | grep -F 'delta-intersect'

prod-stats:
	# Show high-level production stats from feeder + follower
	docker compose logs -f feeder follower 2>&1 | grep -E '\[feeder\]\[stats\]|\[follower\]\[stats\]'

# Minimal k8s deploy for production (uses build_prod.sh)
REGISTRY ?= registry.digitalocean.com/fold
IMAGE_NAME ?= fold
IMAGE_TAG ?= latest
FULL_IMAGE := $(REGISTRY)/$(IMAGE_NAME):$(IMAGE_TAG)
NAMESPACE ?= fold
FEED_JOB ?= fold-feed-job
FEED_TIMEOUT ?= 600s

.PHONY: build-prod deploy-prod feed-prod start-prod clean-prod provision-prod teardown-prod build-prod-and-feed pf-start pf-stop pf-status scale-prod-worker

deploy-prod:
	@echo "==> build + deploy (simple)"
	./build_prod.sh

build-prod:
	@echo "==> build (prod image)"
	./build_prod.sh
	@echo "==> waiting for rollouts (180s) for per-component deployments..."
	@sh -c '\
for d in fold-worker fold-feeder fold-follower fold-ingestor; do \
  echo "waiting for rollout $${d}"; \
  kubectl -n $(NAMESPACE) rollout status deployment/$${d} --timeout=180s || true; \
done'

# Separate target that runs build-prod and then feed-prod for the full flow.
build-prod-and-feed: build-prod
	$(MAKE) feed-prod

feed-prod:
	# Run full production feed sequence: upload, split (in-cluster), feed parts, then tail logs.
	# Usage: just run `make feed-prod` (feeds e.txt). To override: FILE=other.txt make feed-prod
	FILE=e.txt ./feed_prod.sh

start-prod:
	# 1) provision DOKS cluster (creates cluster and attaches registry)
	# Ensure a destructive clean step runs first to avoid stale secrets/PVCs
	./teardown_prod.sh || true
	# Provision will source passwords.sh and create fold-secrets
	./provision_prod.sh
	# provisioning uses doctl --wait so kubeconfig/context should be ready now
	# build-prod-and-feed will build the prod image and then run feed-prod
	$(MAKE) build-prod-and-feed
	# Start local port-forwards for Jaeger and RabbitMQ for developer access
	# (local-only, bound to 127.0.0.1)
	$(MAKE) pf-start || true

# Convenience targets for the provision/teardown steps so everything can be run via make
provision-prod:
	./provision_prod.sh

teardown-prod:
	./teardown_prod.sh

clean-prod:
	@echo "==> running teardown for prod (destructive)"
	./teardown_prod.sh

# Port-forward helpers for local developer access to Jaeger and RabbitMQ
PF_DIR := /tmp/fold-pf
JAEGER_NS ?= default
JAEGER_SVC ?= jaeger-query
JAEGER_PORT ?= 16686
RABBIT_NS ?= default
RABBIT_SVC ?= rabbit-k-rabbitmq
RABBIT_PORT ?= 15672
JAEGER_PID := $(PF_DIR)/jaeger.pid
RABBIT_PID := $(PF_DIR)/rabbit.pid
JAEGER_LOG := $(PF_DIR)/jaeger.log
RABBIT_LOG := $(PF_DIR)/rabbit.log

.PHONY: pf-start pf-stop pf-status
pf-start:
	@mkdir -p $(PF_DIR)
	@echo "Starting jaeger port-forward (localhost:$(JAEGER_PORT))..."
	@if lsof -i :$(JAEGER_PORT) >/dev/null 2>&1; then echo "port $(JAEGER_PORT) already in use"; else \
		nohup kubectl -n $(JAEGER_NS) port-forward svc/$(JAEGER_SVC) $(JAEGER_PORT):$(JAEGER_PORT) --address=127.0.0.1 >$(JAEGER_LOG) 2>&1 & echo $$! > $(JAEGER_PID); \
		echo "jaeger pid: $$(cat $(JAEGER_PID))"; \
	fi
	@echo "Starting rabbitmq port-forward (localhost:$(RABBIT_PORT))..."
	@if lsof -i :$(RABBIT_PORT) >/dev/null 2>&1; then echo "port $(RABBIT_PORT) already in use"; else \
		nohup kubectl -n $(RABBIT_NS) port-forward svc/$(RABBIT_SVC) $(RABBIT_PORT):$(RABBIT_PORT) --address=127.0.0.1 >$(RABBIT_LOG) 2>&1 & echo $$! > $(RABBIT_PID); \
		echo "rabbit pid: $$(cat $(RABBIT_PID))"; \
	fi

pf-stop:
	@for p in $(JAEGER_PID) $(RABBIT_PID); do \
		if [ -f $$p ]; then pid=$$(cat $$p); echo "Killing $$pid..."; kill $$pid 2>/dev/null || true; rm -f $$p; fi; \
	done
	@echo "Stopped port-forwards (logs in $(PF_DIR)/*.log)"

pf-status:
	@echo "Status files in $(PF_DIR):"; ls -l $(PF_DIR) || true