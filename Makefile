start:
	${MAKE} reset
	sleep 15
	./feed.sh

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

clean:
	rm -rf $(INTERNER_FILE_LOCATION)/*
	@echo "Cleaned interner files in $(INTERNER_FILE_LOCATION)"

setup-s3:
	mc alias set localminio http://localhost:9000 minioadmin minioadmin || true

list-s3:
	mc ls localminio/internerdata

split:
	# Usage: make split FILE=yourfile.txt DELIM="\n"
	docker compose run --rm fold_worker /app/s3_util ingest-s3-split s3://internerdata/$(FILE) $(DELIM)

queue-count:
	docker compose run --rm fold_worker /app/queue_checker

db-count:
	docker compose run --rm feeder /app/db_checker database

optimal:
	docker compose run --rm feeder /app/db_checker print-optimal

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
	docker compose run --rm fold_worker /app/s3_util clean-s3-small $(SIZE)

feed-s3:
	docker compose run --rm fold_worker /app/interner_util feed-s3 s3://internerdata/$(FILE)

help-ingestor:
	@echo "Ingestor replaced with individual utilities:"
	@echo "  queue_checker - Check queue depths"
	@echo "  db_checker database - Check database size"
	@echo "  db_checker print-optimal - Print optimal ortho"
	@echo "  db_checker version-counts - Show version counts"
	@echo "  interner_util interner-versions - Show interner versions"
	@echo "  interner_util feed-s3 - Feed from S3"
	@echo "  s3_util ingest-s3-split - Split S3 objects"
	@echo "  s3_util clean-s3-small - Clean small S3 objects"

interner-versions:
	docker compose run --rm fold_worker /app/interner_util interner-versions

version-counts:
	docker compose run --rm feeder /app/db_checker version-counts

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

# Kubernetes deployment targets - simplified workflow like polyvinyl-acetate

.PHONY: k8s-provision k8s-build-deploy k8s-start k8s-feed k8s-monitor k8s-status k8s-scale k8s-clean

k8s-provision:
	# Provision DOKS cluster only (ultra-simple like polyvinyl-acetate)
	./provision.sh

k8s-build-deploy:
	# Combined build and deploy (like polyvinyl-acetate)
	./build-deploy.sh

k8s-start:
	# Complete workflow: provision + build-deploy
	./start.sh

k8s-feed:
	# Feed data to the deployed application
	./feed.sh

k8s-monitor:
	# Monitor the deployed application
	./monitor.sh

k8s-status:
	# Show Kubernetes deployment status
	kubectl get pods -n $(NAMESPACE)
	kubectl get deployments -n $(NAMESPACE)

k8s-scale:
	# Scale worker deployment (usage: REPLICAS=3 make k8s-scale)
	kubectl scale deployment fold-worker --replicas=$(REPLICAS) -n $(NAMESPACE)

k8s-clean:
	# Clean up entire cluster (like polyvinyl-acetate)
	./cleanup.sh