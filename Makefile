up:
	docker-compose up --build -d
	docker-compose logs -f fold

down:
	docker-compose down -v
	docker volume rm fold_miniostorage || true

reset:
	docker-compose down -v
	docker-compose up --build -d
	docker-compose logs -f fold

local:
	cargo run --release