use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use anyhow::Result;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub exp: i64,
}

pub struct TokenBundle {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

pub fn generate_tokens(user_id: Uuid, secret: &str) -> Result<TokenBundle> {
    let access_exp = Utc::now() + Duration::seconds(3600);
    let refresh_exp = Utc::now() + Duration::days(30);

    let access_claims = Claims {
        sub: user_id,
        exp: access_exp.timestamp(),
    };
    let refresh_claims = Claims {
        sub: user_id,
        exp: refresh_exp.timestamp(),
    };

    let access_token = encode(
        &Header::default(),
        &access_claims,
        &EncodingKey::from_secret(secret.as_ref()),
    )?;

    let refresh_token = encode(
        &Header::default(),
        &refresh_claims,
        &EncodingKey::from_secret(secret.as_ref()),
    )?;

    Ok(TokenBundle {
        access_token,
        refresh_token,
        expires_in: 3600,
    })
}

pub fn generate_access_token(user_id: Uuid, secret: &str) -> Result<(String, u64)> {
    let access_exp = Utc::now() + Duration::seconds(3600);
    let access_claims = Claims {
        sub: user_id,
        exp: access_exp.timestamp(),
    };

    let access_token = encode(
        &Header::default(),
        &access_claims,
        &EncodingKey::from_secret(secret.as_ref()),
    )?;

    Ok((access_token, 3600))
}

pub fn verify_token(token: &str, secret: &str) -> Result<Claims> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_ref()),
        &Validation::default(),
    )?;
    Ok(token_data.claims)
}
