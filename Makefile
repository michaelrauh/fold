start:
	${MAKE} reset
	sleep 15
	./feed.sh

build:
	docker build -t fold-services:latest -f Dockerfile .

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

follower-perf:
	# Follow follower perf iteration and window logs
	docker compose logs -f follower 2>&1 | grep -E '\[follower\]\[perf-(iter|window)'

worker-perf:
	# Follow worker perf iteration and window logs
	docker compose logs -f fold_worker 2>&1 | grep -E '\[worker\]\[perf-(iter|window)'

perf-all:
	# Follow both follower and worker perf logs
	docker compose logs -f follower fold_worker 2>&1 | grep -E '\[(follower|worker)\]\[perf-(iter|window)'

prod-stats:
	# Show high-level production stats from feeder + follower
	docker compose logs -f feeder follower 2>&1 | grep -E '\[feeder\]\[stats\]|\[follower\]\[stats\]'