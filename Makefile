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

settlement:
	cargo run -p settlement-service

cex: docker-up
	@echo "Starting services..."
	make auth & \
	make order & \
	make wallet & \
	make matching & \
	make settlement & \
	wait

topics:
	for t in orders.commands engine.events; do \
		docker exec cex-kafka /opt/kafka/bin/kafka-topics.sh --bootstrap-server localhost:9092 \
			--create --if-not-exists --topic $$t --partitions 3 --replication-factor 1; \
	done
