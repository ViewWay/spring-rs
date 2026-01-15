use crate::message::ConnectionId;
use crate::room::RoomId;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// JWT claims for WebSocket authentication
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WsClaims {
    /// Connection ID (issued after connection)
    pub connection_id: Option<ConnectionId>,
    /// Subject (user identifier)
    pub sub: String,
    /// Custom rooms the user can join
    pub rooms: Vec<RoomId>,
    /// Custom metadata
    #[serde(flatten)]
    pub metadata: HashMap<String, serde_json::Value>,
    /// Issued at
    pub iat: u64,
    /// Expiration
    pub exp: u64,
}

/// Authentication service
pub struct AuthService {
    secret: String,
    decoding_key: DecodingKey,
    validation: Validation,
}

impl Clone for AuthService {
    fn clone(&self) -> Self {
        Self {
            secret: self.secret.clone(),
            decoding_key: DecodingKey::from_secret(self.secret.as_ref()),
            validation: self.validation.clone(),
        }
    }
}

impl AuthService {
    /// Create a new auth service from a JWT secret
    pub fn new(jwt_secret: &str) -> Self {
        let decoding_key = DecodingKey::from_secret(jwt_secret.as_ref());
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;

        Self {
            secret: jwt_secret.to_string(),
            decoding_key,
            validation,
        }
    }

    /// Validate a JWT token and return the claims
    pub fn validate_token(&self, token: &str) -> Result<WsClaims, AuthError> {
        let token_data = decode::<WsClaims>(token, &self.decoding_key, &self.validation)
            .map_err(|e| AuthError::InvalidToken(e.to_string()))?;

        Ok(token_data.claims)
    }

    /// Generate a JWT token for a user
    pub fn generate_token(
        &self,
        user_id: &str,
        rooms: Vec<RoomId>,
        exp_seconds: u64,
    ) -> Result<String, AuthError> {
        let encoding_key = EncodingKey::from_secret(self.secret.as_ref());
        let now = jsonwebtoken::get_current_timestamp();
        let claims = WsClaims {
            connection_id: None,
            sub: user_id.to_string(),
            rooms,
            metadata: HashMap::new(),
            iat: now,
            exp: now + exp_seconds,
        };

        encode(
            &Header::default(),
            &claims,
            &encoding_key,
        )
        .map_err(|e| AuthError::TokenGeneration(e.to_string()))
    }

    /// Generate a token with custom metadata
    pub fn generate_token_with_metadata(
        &self,
        user_id: &str,
        rooms: Vec<RoomId>,
        exp_seconds: u64,
        metadata: HashMap<String, serde_json::Value>,
    ) -> Result<String, AuthError> {
        let encoding_key = EncodingKey::from_secret(self.secret.as_ref());
        let now = jsonwebtoken::get_current_timestamp();
        let claims = WsClaims {
            connection_id: None,
            sub: user_id.to_string(),
            rooms,
            metadata,
            iat: now,
            exp: now + exp_seconds,
        };

        encode(
            &Header::default(),
            &claims,
            &encoding_key,
        )
        .map_err(|e| AuthError::TokenGeneration(e.to_string()))
    }
}

/// Authenticated connection info
#[derive(Clone, Debug)]
pub struct AuthInfo {
    pub user_id: String,
    pub connection_id: ConnectionId,
    pub allowed_rooms: Vec<RoomId>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Authentication errors
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Invalid token: {0}")]
    InvalidToken(String),

    #[error("Token generation failed: {0}")]
    TokenGeneration(String),

    #[error("Missing token")]
    MissingToken,

    #[error("Expired token")]
    ExpiredToken,

    #[error("Access denied to room: {0}")]
    AccessDenied(RoomId),
}

/// In-memory session store for authenticated connections
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<ConnectionId, AuthInfo>>>,
}

impl Clone for SessionStore {
    fn clone(&self) -> Self {
        Self {
            sessions: Arc::clone(&self.sessions),
        }
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_session(&self, connection_id: ConnectionId, auth_info: AuthInfo) {
        let mut sessions = self.sessions.write().await;
        sessions.insert(connection_id, auth_info);
    }

    pub async fn get_session(&self, connection_id: &ConnectionId) -> Option<AuthInfo> {
        let sessions = self.sessions.read().await;
        sessions.get(connection_id).cloned()
    }

    pub async fn remove_session(&self, connection_id: &ConnectionId) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(connection_id);
    }

    pub async fn check_room_access(&self, connection_id: &ConnectionId, room_id: &RoomId) -> Result<(), AuthError> {
        if let Some(auth_info) = self.get_session(connection_id).await {
            if auth_info.allowed_rooms.iter().any(|r| r == room_id || r == "*") {
                Ok(())
            } else {
                Err(AuthError::AccessDenied(room_id.clone()))
            }
        } else {
            // No auth info - allow access (backward compatibility)
            Ok(())
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "test_secret_key_for_jwt";

    #[test]
    fn test_token_generation_and_validation() {
        let auth = AuthService::new(TEST_SECRET);

        let user_id = "user123";
        let rooms = vec!["room1".to_string(), "room2".to_string()];

        let token = auth.generate_token(user_id, rooms.clone(), 3600u64).unwrap();
        let claims = auth.validate_token(&token).unwrap();

        assert_eq!(claims.sub, user_id);
        assert_eq!(claims.rooms, rooms);
    }

    #[test]
    fn test_invalid_token() {
        let auth = AuthService::new(TEST_SECRET);
        let result = auth.validate_token("invalid_token");
        assert!(result.is_err());
    }

    #[test]
    fn test_session_store() {
        let store = SessionStore::new();
        let connection_id = uuid::Uuid::new_v4();

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let auth_info = AuthInfo {
                user_id: "user123".to_string(),
                connection_id,
                allowed_rooms: vec!["room1".to_string()],
                metadata: HashMap::new(),
            };

            store.add_session(connection_id, auth_info.clone()).await;
            let retrieved = store.get_session(&connection_id).await;
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().user_id, "user123");
        });
    }
}
