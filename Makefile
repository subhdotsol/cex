docker-up:
	docker compose up -d

migrate:
	sqlx migrate run --source crates/db/migrations

auth:
	cargo run -p auth-service

order:
	cargo run -p order-service

wallet:
	cargo run -p wallet-service

matching:
	cargo run -p matching-engine

cex: docker-up
	@echo "Starting services..."
	make auth & \
	make order & \
	make wallet & \
	make matching & \
	wait
