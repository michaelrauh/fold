up:
	docker-compose up --build -d
	docker-compose logs -f fold

down:
	docker-compose down -v

reset:
	docker-compose down -v
	docker-compose up --build -d
	docker-compose logs -f fold

local:
	cargo run --release