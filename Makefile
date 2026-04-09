docker-up:
	docker compose up -d

auth:
	cargo run -p auth-service

order:
	cargo run -p order-service

wallet:
	cargo run -p wallet-service

cex: docker-up
	@echo "Starting services..."
	make auth & \
	make order & \
	make wallet & \
	wait
