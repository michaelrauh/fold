start:
	${MAKE} reset
	sleep 10
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

make scale:
	docker compose up --scale fold_worker=$(REPLICAS) -d

make stats:
	docker stats