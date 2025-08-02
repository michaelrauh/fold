up:
	RUST_LOG=info,fold=trace,aws_sdk_s3=warn,aws_smithy_runtime=warn,hyper=warn,opentelemetry=warn docker-compose up --build -d
	docker-compose logs -f ingestor fold_worker feeder follower | grep -E '\[main|\[worker|\[follower|\[feeder|\[queue|\[ortho|\[interner' || true

down:
	docker-compose down -v

reset:
	$(MAKE) down
	$(MAKE) up

local:
	RUST_LOG=info,fold=trace cargo run --release --bin fold

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
	# Usage: make logs SERVICE=fold
	docker compose logs -f $(SERVICE)

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