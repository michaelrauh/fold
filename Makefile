up:
	docker-compose up --build -d
	docker-compose logs -f fold

down:
	docker-compose down

local:
	cargo run --release