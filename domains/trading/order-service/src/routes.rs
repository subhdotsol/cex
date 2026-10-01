use actix_web::{HttpResponse, Responder, delete, get, post, web};
use chrono::Utc;
use db::Decimal;
use db::balances;
use db::orders::{self, DbOrderStatus};
use deadpool_redis::Pool as RedisPool;
use risk_engine::RiskConfig;
use kafka::FutureProducer;
use kafka::order::{OrderCancel, OrderNew, produce_order_cancel, produce_order_new};
use sqlx::PgPool;
use utils::auth::AuthUser;
use types::{
    GetAllOrdersRequest, OrderCancelResponse, OrderHealthCheckResponse, OrderRequest,
    OrderResponse, OrderStatus,
};
use uuid::Uuid;

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/orders")
            .service(place_order)
            .service(get_all_orders)
            .service(health_check)
            .service(get_order)
            .service(cancel_order),
    );
}

#[post("")]
async fn place_order(
    auth: AuthUser,
    pg_pool: web::Data<PgPool>,
    redis_pool: web::Data<RedisPool>,
    kafka_producer: web::Data<FutureProducer>,
    risk: web::Data<RiskConfig>,
    body: web::Json<OrderRequest>,
) -> impl Responder {
    let user_id = auth.0;

    // 1. Pre-trade risk checks + how much to reserve
    let assessment = match risk_engine::assess(&risk, &pg_pool, &redis_pool, user_id, &body).await {
        Ok(a) => a,
        Err(e) if e.is_client_error() => return HttpResponse::BadRequest().body(e.to_string()),
        Err(e) => {
            eprintln!("Risk check failed: {e}");
            return HttpResponse::InternalServerError().finish();
        }
    };

    // 2. Reserve funds + insert order atomically
    let order_id = Uuid::new_v4();
    let mut tx = match pg_pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            eprintln!("Failed to begin transaction: {e:?}");
            return HttpResponse::InternalServerError().finish();
        }
    };

    let balance = match balances::lock(
        &mut tx,
        user_id,
        &assessment.lock_asset,
        assessment.lock_amount,
        order_id,
    )
    .await
    {
        Ok(Some(b)) => b,
        Ok(None) => return HttpResponse::BadRequest().body("insufficient balance"),
        Err(e) => {
            eprintln!("Failed to lock funds: {e:?}");
            return HttpResponse::InternalServerError().finish();
        }
    };

    if let Err(e) = orders::create_order_with_lock(
        &mut tx,
        order_id,
        user_id,
        &body.pair,
        body.side,
        body.order_type,
        body.price.as_deref(),
        &body.qty,
        assessment.lock_amount,
    )
    .await
    {
        eprintln!("Failed to save order: {e:?}");
        return HttpResponse::InternalServerError().body("Failed to save order");
    }

    if let Err(e) = tx.commit().await {
        eprintln!("Failed to commit order: {e:?}");
        return HttpResponse::InternalServerError().finish();
    }

    redis_client::publish_balance(
        &redis_pool,
        user_id,
        &balance.asset,
        balance.available.normalize().to_string(),
        "LOCK",
    )
    .await;

    // 3. Hand over to the matching engine
    let created_at = Utc::now();
    let kafka_event = OrderNew {
        order_id,
        user_id,
        pair: body.pair.clone(),
        side: format!("{:?}", body.side).to_uppercase(),
        order_type: format!("{:?}", body.order_type).to_uppercase(),
        price: assessment.order.price.map(|p| p.normalize().to_string()),
        qty: assessment.order.qty.normalize().to_string(),
        timestamp: created_at,
    };

    if let Err(e) = produce_order_new(&kafka_producer, &kafka_event).await {
        // The engine never saw it: reject the order and give the funds back.
        eprintln!("Failed to produce kafka message: {e:?}");
        reject_unsent_order(&pg_pool, &redis_pool, user_id, order_id, &assessment).await;
        return HttpResponse::ServiceUnavailable().body("order could not be submitted, try again");
    }

    HttpResponse::Ok().json(OrderResponse {
        order_id,
        status: OrderStatus::Open,
        pair: body.pair.clone(),
        side: body.side,
        price: body.price.clone(),
        qty: body.qty.clone(),
        filled_qty: "0".to_string(),
        created_at,
    })
}

async fn reject_unsent_order(
    pg_pool: &PgPool,
    redis_pool: &RedisPool,
    user_id: Uuid,
    order_id: Uuid,
    assessment: &risk_engine::Assessment,
) {
    let result: Result<_, sqlx::Error> = async {
        let mut tx = pg_pool.begin().await?;
        orders::set_fill_state(&mut tx, order_id, Decimal::ZERO, Decimal::ZERO, DbOrderStatus::Rejected)
            .await?;
        let b = balances::adjust(
            &mut tx,
            user_id,
            &assessment.lock_asset,
            assessment.lock_amount,
            -assessment.lock_amount,
            balances::kind::UNLOCK,
            Some(order_id),
        )
        .await?;
        tx.commit().await?;
        Ok(b)
    }
    .await;

    match result {
        Ok(b) => {
            redis_client::publish_balance(redis_pool, user_id, &b.asset, b.available.normalize().to_string(), "UNLOCK")
                .await
        }
        Err(e) => eprintln!("CRITICAL: failed to release funds of unsent order {order_id}: {e:?}"),
    }
}

#[get("/{order_id}")]
async fn get_order(
    auth: AuthUser,
    pg_pool: web::Data<PgPool>,
    path: web::Path<Uuid>,
) -> impl Responder {
    let user_id = auth.0;
    let order_id = path.into_inner();

    // 2. Fetch from DB
    let order = match db::orders::get_single_order(&pg_pool, order_id).await {
        Ok(Some(o)) => o,
        Ok(None) => return HttpResponse::NotFound().body("Order not found"),
        Err(e) => {
            eprintln!("Failed to fetch order: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    // 3. Ownership Check
    if order.user_id != user_id {
        return HttpResponse::Forbidden().body("Order belongs to different user");
    }

    // 4. Return Order
    let response = OrderResponse {
        order_id: order.id,
        status: order.status.into(),
        pair: order.pair.clone(),
        side: order.side.into(),
        price: order.price.clone(),
        qty: order.qty.clone(),
        filled_qty: order.filled_qty.clone(),
        created_at: order.created_at,
    };

    HttpResponse::Ok().json(response)
}

#[get("")]
async fn get_all_orders(
    auth: AuthUser,
    pg_pool: web::Data<PgPool>,
    query: web::Query<GetAllOrdersRequest>,
) -> impl Responder {
    let user_id = auth.0;

    let limit = query.limit.unwrap_or(50);
    if limit < 1 || limit > 200 {
        return HttpResponse::BadRequest().body("limit must be between 1 and 200");
    }

    // Fetch limit + 1 to check for has_more
    let db_orders = match orders::get_all_orders(
        &pg_pool,
        user_id,
        limit + 1,
        query.status,
        query.pair.clone(),
        query.before,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to fetch orders: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    let has_more = db_orders.len() > limit;
    let results = if has_more {
        &db_orders[..limit]
    } else {
        &db_orders[..]
    };

    let next_cursor = if has_more {
        results.last().map(|o| o.id)
    } else {
        None
    };

    let orders_response: Vec<OrderResponse> = results
        .iter()
        .map(|o| OrderResponse {
            order_id: o.id,
            status: o.status.into(),
            pair: o.pair.clone(),
            side: o.side.into(),
            price: o.price.clone(),
            qty: o.qty.clone(),
            filled_qty: o.filled_qty.clone(),
            created_at: o.created_at,
        })
        .collect();

    let response = types::PaginatedOrders {
        orders: orders_response,
        next_cursor,
        has_more,
    };

    HttpResponse::Ok().json(response)
}

#[get("/health")]
async fn health_check() -> impl Responder {
    let response = OrderHealthCheckResponse {
        status: "ok".to_string(),
        service: "order-service".to_string(),
        version: "0.1.0".to_string(),
    };
    HttpResponse::Ok().json(response)
}

#[delete("/{order_id}")]
async fn cancel_order(
    auth: AuthUser,
    pg_pool: web::Data<PgPool>,
    kafka_producer: web::Data<FutureProducer>,
    path: web::Path<Uuid>,
) -> impl Responder {
    let user_id = auth.0;
    let order_id = path.into_inner();

    // 2. Fetch Order from Database
    let order = match db::orders::get_single_order(&pg_pool, order_id).await {
        Ok(Some(o)) => o,
        Ok(None) => return HttpResponse::NotFound().body("Order not found"),
        Err(e) => {
            eprintln!("Failed to fetch order: {:?}", e);
            return HttpResponse::InternalServerError().finish();
        }
    };

    // 3. Ownership Check (403)
    if order.user_id != user_id {
        return HttpResponse::Forbidden().body("Order belongs to different user");
    }

    // 4. Status Validation (400)
    if order.status != DbOrderStatus::Open && order.status != DbOrderStatus::PartiallyFilled {
        return HttpResponse::BadRequest().body("Order is already filled, cancelled or rejected");
    }

    // 5. Side Effect: UPDATE orders SET status=CANCELLED
    if let Err(e) = db::orders::update_order_status(&pg_pool, order_id, DbOrderStatus::Cancelled).await {
        eprintln!("Failed to update order status: {:?}", e);
        return HttpResponse::InternalServerError().finish();
    }

    // 6. Funds are released by the settlement service once the matching engine
    //    confirms the order left the book (it may still fill until then).

    // 7. Side Effect: Produce orders.cancel to Kafka
    let kafka_event = OrderCancel {
        order_id,
        user_id,
        pair: order.pair.clone(),
        timestamp: Utc::now(),
    };

    if let Err(e) = produce_order_cancel(&kafka_producer, &kafka_event).await {
        eprintln!("Failed to produce kafka cancel message: {:?}", e);
    }

    // 8. Response 200
    let response = OrderCancelResponse {
        order_id,
        status: OrderStatus::Cancelled,
    };

    HttpResponse::Ok().json(response)
}
