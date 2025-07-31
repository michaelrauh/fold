up:
	RUST_LOG=info,fold=trace docker-compose up --build -d
	# Print only your logs to stdout, but all logs (including dependencies) are exported to Jaeger via the tracing config in your app
	docker-compose logs -f fold | grep -E '\[main|\[worker|\[follower|\[feeder|\[queue|\[ortho|\[interner' || true

down:
	docker-compose down -v
	docker volume rm fold_miniostorage || true

reset:
	docker-compose down -v
	docker-compose up --build -d
	docker-compose logs -f fold

local:
	RUST_LOG=info,fold=trace cargo run --release