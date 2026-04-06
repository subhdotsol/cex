use actix_web::{App, HttpServer};

mod routes;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    println!("Starting auth-service on 127.0.0.1:8083");
    HttpServer::new(|| App::new().configure(routes::init))
        .bind(("127.0.0.1", 8083))?
        .run()
        .await
}
