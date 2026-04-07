use deadpool_redis::{Config, Pool, Runtime};

pub fn create_pool(url: &str) -> Pool {
    let cfg = Config::from_url(url);
    cfg.create_pool(Some(Runtime::Tokio1))
        .expect("Failed to create redis pool")
}
