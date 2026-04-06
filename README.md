# Centralized Exchange Backend Architecture

## Overview
This repository contains a high performance centralized exchange backend implemented in Rust. The system is designed for low latency order execution, strong consistency in financial operations, and horizontal scalability across services. The architecture follows an event driven model using a distributed log to ensure durability, replayability, and fault tolerance.

## Core Principles
The system is built around deterministic processing, minimal latency in the matching path, and clear separation of responsibilities between services. Critical trading logic is kept in memory for speed, while persistence and downstream processing are handled asynchronously through event streams.

## System Architecture
<img width="1675" height="1148" alt="image" src="https://github.com/user-attachments/assets/2d33510d-4e59-4915-8397-e6ef861dcf75" />

## Schema
<img width="950" height="728" alt="Screenshot 2026-04-04 at 6 19 11 PM" src="https://github.com/user-attachments/assets/dad39610-bc16-4411-887f-7f4f1354abb1" />

## CEX API endpoints
[api-endpoints](https://subhisgreat.notion.site/CEX-api-endpoints-33917f1447c180e8af7ffc0fc620f9b5)

### Client Layer
A web user interface built with Next.js interacts with the backend through a secured API gateway. All requests pass through authentication and rate limiting before entering internal services.

### API Gateway and Authentication
The API gateway handles TLS termination, routing, and rate limiting. The authentication service manages JSON Web Tokens, API keys, and optional two factor authentication. This layer ensures that only authorized requests reach internal systems.

### Event Streaming Layer
A Kafka cluster acts as the backbone of the system. All state changes are emitted as events including new orders, fills, executed trades, and balance updates. Topics are durable and replayable, allowing services to recover state or scale independently.

## Core Services

### Matching Engine
Implemented in Rust and running fully in memory, the matching engine is responsible for order book management and trade execution. It consumes order events and produces execution results with minimal latency.

### Order Service
Validates incoming orders and persists them for auditability. It ensures correctness before forwarding orders to the matching engine.

### Risk Engine
Enforces trading limits and monitors exposure. It evaluates orders before execution to prevent invalid or risky trades.

### Wallet Service
Manages deposits and withdrawals. It updates balances and emits balance events that propagate through the system.

### Notification Service
Handles outbound communication such as email, push notifications, and websocket updates to clients.

## Data Layer

### Redis Cluster
Used for fast access to order book snapshots, session tokens, and balance caching. It also provides publish and subscribe capabilities for websocket fanout.

### PostgreSQL
Stores users, orders, trades, balances, and audit logs. It serves as the primary source of truth for transactional data.

### Time Series Database
Stores OHLCV candle data and tick level market history for analytics and charting.

### Object Storage
Used for trade reports, account statements, and compliance exports.

## Realtime Delivery

### Websocket Gateway
Horizontally scalable websocket pods subscribe to Redis channels and push real time updates such as order book depth, trades, and fills to connected clients. Sticky sessions ensure consistent routing for active connections.

### Balance Service and Pub Sub
Balance updates are published to Redis and broadcast to websocket nodes, enabling near real time account state visibility with sub millisecond read performance.

## Infrastructure

### Containerization and Orchestration
Local development is supported via Docker Compose. Production workloads run on Kubernetes with horizontal pod autoscaling for API and websocket services. Stateful components such as Kafka are deployed using StatefulSets.

### Ingress and Load Balancing
External traffic is routed through an ingress controller and load balancer, distributing requests across API gateway pods, websocket pods, and other services.

### Observability
Monitoring is handled using Prometheus and Grafana. Logging is centralized with Loki, and distributed tracing is implemented using Jaeger. This provides full visibility into request flows and system performance.

### Continuous Integration and Deployment
Build and deployment pipelines are managed using GitHub Actions. Continuous delivery is implemented with Argo CD and Helm charts to ensure reproducible and controlled releases.

## Request Flow Example

1. A client submits an order through the API gateway.  
2. The order is authenticated and validated by the order service.  
3. The order event is published to Kafka.  
4. The matching engine consumes the event and executes trades.  
5. Execution results are emitted back into Kafka.  
6. Wallet and balance services update account states.  
7. Redis propagates updates to websocket gateways.  
8. Clients receive real time updates through persistent connections.

## Scalability and Fault Tolerance

Stateless services scale horizontally behind the load balancer.  
Stateful components rely on replication and partitioning.  
Kafka ensures durability and replay capability for all critical events.  
Redis enables low latency data access and pub sub messaging.  
Services can recover state by replaying event streams.

## Security Considerations

All external communication is secured with TLS.  
Authentication uses token based mechanisms with optional two factor support.  
Rate limiting protects against abuse and denial of service attacks.  
Audit logs are maintained for all financial operations.

## Conclusion

This architecture is designed to meet the demands of a modern centralized exchange, balancing performance, reliability, and scalability. Rust provides the necessary performance guarantees for latency sensitive components, while the event driven design ensures resilience and extensibility across the platform.
