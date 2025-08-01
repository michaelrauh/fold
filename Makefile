up:
	RUST_LOG=info,fold=trace,aws_sdk_s3=warn,aws_smithy_runtime=warn,hyper=warn,opentelemetry=warn docker-compose up --build -d
	# Print logs for all services to stdout, but all logs (including dependencies) are exported to Jaeger via the tracing config in your app
	docker-compose logs -f fold fold_worker feeder follower | grep -E '\[main|\[worker|\[follower|\[feeder|\[queue|\[ortho|\[interner' || true

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