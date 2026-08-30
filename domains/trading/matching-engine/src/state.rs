use chrono::{DateTime, Utc};
use ordered_float::NotNan;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct BookOrder {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub remaining_qty: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct OrderBook {
    pub bids: BTreeMap<Reverse<NotNan<f64>>, VecDeque<BookOrder>>,
    pub asks: BTreeMap<NotNan<f64>, VecDeque<BookOrder>>,
    // order_id -> (side, price) for O(1) cancel lookup
    pub index: HashMap<Uuid, (Side, f64)>,
}

impl OrderBook {
    pub fn depth_snapshot(&self, limit: usize) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
        let bids = self
            .bids
            .iter()
            .take(limit)
            .map(|(Reverse(p), level)| {
                let qty: f64 = level.iter().map(|o| o.remaining_qty).sum();
                ((*p).into_inner(), qty)
            })
            .collect();

        let asks = self
            .asks
            .iter()
            .take(limit)
            .map(|(p, level)| {
                let qty: f64 = level.iter().map(|o| o.remaining_qty).sum();
                ((*p).into_inner(), qty)
            })
            .collect();

        (bids, asks)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicTrade {
    pub price: String,
    pub qty: String,
    pub side: String,
    pub executed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct Ticker24h {
    pub last_price: f64,
    pub open_price: f64,
    pub high_24h: f64,
    pub low_24h: f64,
    pub volume_24h: f64,
    pub updated_at: DateTime<Utc>,
}

pub struct EngineState {
    pub books: HashMap<String, OrderBook>,
    pub recent_trades: HashMap<String, VecDeque<PublicTrade>>,
    pub ticker: HashMap<String, Ticker24h>,
    pub kafka_lag: i64,
}

impl EngineState {
    pub fn new(pairs: &[&str]) -> Self {
        let mut books = HashMap::new();
        let mut recent_trades = HashMap::new();
        let mut ticker = HashMap::new();
        for &pair in pairs {
            books.insert(pair.to_string(), OrderBook::default());
            recent_trades.insert(pair.to_string(), VecDeque::with_capacity(200));
            ticker.insert(pair.to_string(), Ticker24h::default());
        }
        Self {
            books,
            recent_trades,
            ticker,
            kafka_lag: 0,
        }
    }

    pub fn orders_in_book(&self) -> usize {
        self.books.values().map(|b| b.index.len()).sum()
    }
}

pub type SharedState = Arc<RwLock<EngineState>>;
