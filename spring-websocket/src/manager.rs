use crate::config::WebSocketConfig;
use crate::message::{ConnectionId, WebSocketMessage, MessageType};
use crate::room::{RoomManager, RoomId};
use axum::extract::ws::{Message, WebSocket};
use dashmap::DashMap;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use uuid::Uuid;

/// Rate limiter for connections
#[derive(Clone)]
pub struct RateLimiter {
    message_count: Arc<AtomicUsize>,
    window_start: Arc<tokio::sync::RwLock<Instant>>,
    max_messages: usize,
    window_duration: Duration,
}

impl RateLimiter {
    pub fn new(max_messages: usize, window_duration: Duration) -> Self {
        Self {
            message_count: Arc::new(AtomicUsize::new(0)),
            window_start: Arc::new(tokio::sync::RwLock::new(Instant::now())),
            max_messages,
            window_duration,
        }
    }

    /// Check if a message is allowed under the rate limit
    pub async fn check(&self) -> bool {
        let mut start = self.window_start.write().await;
        let now = Instant::now();

        // Reset window if expired
        if now.duration_since(*start) >= self.window_duration {
            *start = now;
            self.message_count.store(0, Ordering::Relaxed);
        }

        // Check and increment
        let count = self.message_count.fetch_add(1, Ordering::Relaxed);
        count < self.max_messages
    }

    /// Reset the rate limiter
    pub async fn reset(&self) {
        self.message_count.store(0, Ordering::Relaxed);
        *self.window_start.write().await = Instant::now();
    }
}

/// WebSocket connection state
#[derive(Debug)]
pub struct ConnectionState {
    /// Connection ID
    pub id: ConnectionId,
    /// Client address
    pub addr: SocketAddr,
    /// Connection start time
    pub connected_at: Instant,
    /// Last ping time
    pub last_ping: Option<Instant>,
    /// Last pong time
    pub last_pong: Option<Instant>,
    /// Joined rooms
    pub rooms: Vec<RoomId>,
    /// User data associated with connection
    pub user_data: std::collections::HashMap<String, String>,
    /// Connection statistics
    pub stats: ConnectionStats,
}

/// Connection statistics
#[derive(Debug, Default)]
pub struct ConnectionStats {
    /// Messages sent
    pub messages_sent: AtomicUsize,
    /// Messages received
    pub messages_received: AtomicUsize,
    /// Bytes sent
    pub bytes_sent: AtomicUsize,
    /// Bytes received
    pub bytes_received: AtomicUsize,
}

impl ConnectionState {
    /// Create a new connection state
    pub fn new(id: ConnectionId, addr: SocketAddr) -> Self {
        Self {
            id,
            addr,
            connected_at: Instant::now(),
            last_ping: None,
            last_pong: None,
            rooms: Vec::new(),
            user_data: std::collections::HashMap::new(),
            stats: ConnectionStats::default(),
        }
    }

    /// Check if connection is stale (no pong received for a while)
    pub fn is_stale(&self, timeout: Duration) -> bool {
        if let Some(last_pong) = self.last_pong {
            Instant::now().duration_since(last_pong) > timeout
        } else {
            // If we haven't received any pong, consider it stale after connection timeout
            Instant::now().duration_since(self.connected_at) > timeout
        }
    }

    /// Update ping timestamp
    pub fn update_ping(&mut self) {
        self.last_ping = Some(Instant::now());
    }

    /// Update pong timestamp
    pub fn update_pong(&mut self) {
        self.last_pong = Some(Instant::now());
    }

    /// Join a room
    pub fn join_room(&mut self, room_id: RoomId) {
        if !self.rooms.contains(&room_id) {
            self.rooms.push(room_id);
        }
    }

    /// Leave a room
    pub fn leave_room(&mut self, room_id: &RoomId) {
        self.rooms.retain(|r| r != room_id);
    }

    /// Set user data
    pub fn set_user_data<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
        self.user_data.insert(key.into(), value.into());
    }

    /// Get user data
    pub fn get_user_data(&self, key: &str) -> Option<&String> {
        self.user_data.get(key)
    }
}

/// WebSocket connection manager
pub struct WebSocketManager {
    /// Configuration
    config: WebSocketConfig,
    /// Active connections
    connections: Arc<DashMap<ConnectionId, Arc<RwLock<ConnectionState>>>>,
    /// Connection sender channels
    senders: Arc<DashMap<ConnectionId, mpsc::UnboundedSender<Message>>>,
    /// Room manager
    pub room_manager: RoomManager,
    /// Active connection count
    connection_count: AtomicUsize,
    /// Message handlers
    message_handlers: Arc<DashMap<MessageType, Box<dyn MessageHandler + Send + Sync>>>,
    /// Rate limiter per connection
    rate_limiters: Arc<DashMap<ConnectionId, Arc<RateLimiter>>>,
}

/// Wrapper for Arc<dyn MessageHandler>
struct MessageHandlerWrapper {
    handler: Arc<dyn MessageHandler + Send + Sync>,
}

impl MessageHandler for MessageHandlerWrapper {
    fn handle(
        &self,
        message: WebSocketMessage,
        connection_id: ConnectionId,
        manager: &WebSocketManager,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.handler.handle(message, connection_id, manager)
    }
}

impl Clone for MessageHandlerWrapper {
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
        }
    }
}

/// Message handler trait
pub trait MessageHandler: Send + Sync {
    /// Handle a message
    fn handle(
        &self,
        message: WebSocketMessage,
        connection_id: ConnectionId,
        manager: &WebSocketManager,
    ) -> Result<(), Box<dyn std::error::Error>>;
}

/// Context for handling WebSocket messages
struct MessageHandlerContext {
    connection_id: ConnectionId,
    connection_state: Arc<RwLock<ConnectionState>>,
    connections: Arc<DashMap<ConnectionId, Arc<RwLock<ConnectionState>>>>,
    senders: Arc<DashMap<ConnectionId, mpsc::UnboundedSender<Message>>>,
    handlers: Arc<DashMap<MessageType, Box<dyn MessageHandler + Send + Sync>>>,
    config: WebSocketConfig,
    connection_count: Arc<AtomicUsize>,
    room_manager: RoomManager,
    rate_limiters: Arc<DashMap<ConnectionId, Arc<RateLimiter>>>,
}

impl MessageHandlerContext {
    fn new(
        connection_id: ConnectionId,
        connection_state: Arc<RwLock<ConnectionState>>,
        connections: Arc<DashMap<ConnectionId, Arc<RwLock<ConnectionState>>>>,
        senders: Arc<DashMap<ConnectionId, mpsc::UnboundedSender<Message>>>,
        handlers: Arc<DashMap<MessageType, Box<dyn MessageHandler + Send + Sync>>>,
        config: WebSocketConfig,
        connection_count: Arc<AtomicUsize>,
        room_manager: RoomManager,
        rate_limiters: Arc<DashMap<ConnectionId, Arc<RateLimiter>>>,
    ) -> Self {
        Self {
            connection_id,
            connection_state,
            connections,
            senders,
            handlers,
            config,
            connection_count,
            room_manager,
            rate_limiters,
        }
    }
}

impl WebSocketManager {
    /// Create a new WebSocket manager
    pub fn new(config: WebSocketConfig) -> Self {
        Self {
            config,
            connections: Arc::new(DashMap::new()),
            senders: Arc::new(DashMap::new()),
            room_manager: RoomManager::new(),
            connection_count: AtomicUsize::new(0),
            message_handlers: Arc::new(DashMap::new()),
            rate_limiters: Arc::new(DashMap::new()),
        }
    }

    /// Get rate limit config
    fn get_rate_limit_config(&self) -> Option<(usize, Duration)> {
        self.config.rate_limit.as_ref().map(|rl| {
            (rl.messages_per_minute as usize, Duration::from_secs(60))
        })
    }

    /// Handle new WebSocket connection
    pub async fn handle_connection(
        &self,
        socket: WebSocket,
        addr: SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Check connection limit
        if self.connection_count.load(Ordering::Relaxed) >= self.config.max_connections {
            tracing::warn!("Connection limit reached, rejecting connection from {}", addr);
            return Ok(());
        }

        let connection_id = Uuid::new_v4();
        let connection_state = Arc::new(RwLock::new(ConnectionState::new(connection_id, addr)));

        // Split the socket
        let (ws_sender, mut ws_receiver) = socket.split();

        // Create message channel
        let (message_sender, mut message_receiver) = mpsc::unbounded_channel::<Message>();

        // Store connection info
        self.connections.insert(connection_id, connection_state.clone());
        self.senders.insert(connection_id, message_sender);

        // Initialize rate limiter if configured
        if let Some((max_msgs, window)) = self.get_rate_limit_config() {
            let rate_limiter = Arc::new(RateLimiter::new(max_msgs, window));
            self.rate_limiters.insert(connection_id, rate_limiter);
        }

        self.connection_count.fetch_add(1, Ordering::Relaxed);

        tracing::info!("New WebSocket connection: {} from {}", connection_id, addr);

        // Send welcome message
        let welcome_msg = WebSocketMessage::new_system("Connected to WebSocket server")
            .with_metadata("connection_id", connection_id.to_string());
        self.send_message_to_connection(connection_id, welcome_msg).await?;

        // Handle incoming messages
        let ctx = MessageHandlerContext::new(
            connection_id,
            connection_state.clone(),
            self.connections.clone(),
            self.senders.clone(),
            self.message_handlers.clone(),
            self.config.clone(),
            Arc::new(AtomicUsize::new(self.connection_count.load(Ordering::Relaxed))),
            self.room_manager.clone(),
            self.rate_limiters.clone(),
        );

        tokio::spawn(async move {
            if let Err(e) = Self::handle_messages(
                ctx,
                &mut ws_receiver,
                &mut message_receiver,
                ws_sender,
            ).await {
                tracing::error!("Error handling WebSocket messages for {}: {:?}", connection_id, e);
            }
        });

        Ok(())
    }

    /// Handle incoming WebSocket messages
    async fn handle_messages(
        ctx: MessageHandlerContext,
        ws_receiver: &mut SplitStream<WebSocket>,
        message_receiver: &mut mpsc::UnboundedReceiver<Message>,
        mut ws_sender: SplitSink<WebSocket, Message>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let connection_id = ctx.connection_id;
        let config = ctx.config.clone();
        let senders = ctx.senders.clone();
        let connections = ctx.connections.clone();

        // Start ping task
        let ping_connection_id = connection_id;
        let ping_config = config.clone();
        let ping_senders = senders.clone();
        tokio::spawn(async move {
            let mut ping_interval = tokio::time::interval(ping_config.ping_interval_duration());
            loop {
                ping_interval.tick().await;

                if let Some(sender) = ping_senders.get(&ping_connection_id) {
                    let _ = sender.send(Message::Ping(Vec::new().into()));
                } else {
                    break;
                }
            }
        });

        // Start cleanup task
        let cleanup_config = config.clone();
        let cleanup_connections = connections.clone();
        let cleanup_senders = senders.clone();
        let cleanup_rate_limiters = ctx.rate_limiters.clone();
        let cleanup_room_manager = ctx.room_manager.clone();
        tokio::spawn(async move {
            let mut cleanup_interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                cleanup_interval.tick().await;

                // Check for stale connections
                let stale_connections: Vec<ConnectionId> = cleanup_connections
                    .iter()
                    .filter_map(|entry| {
                        let state = entry.value();
                        if state.try_read().is_ok_and(|s| s.is_stale(cleanup_config.idle_timeout_duration())) {
                            Some(*entry.key())
                        } else {
                            None
                        }
                    })
                    .collect();

                for conn_id in stale_connections {
                    tracing::info!("Closing stale connection: {}", conn_id);
                    Self::close_connection(
                        conn_id,
                        cleanup_connections.clone(),
                        cleanup_senders.clone(),
                        cleanup_rate_limiters.clone(),
                        &cleanup_room_manager,
                    ).await;
                }
            }
        });

        loop {
            tokio::select! {
                // Handle WebSocket messages
                ws_msg = ws_receiver.next() => {
                    match ws_msg {
                        Some(Ok(msg)) => {
                            if let Err(e) = Self::process_websocket_message(
                                msg,
                                connection_id,
                                ctx.connection_state.clone(),
                                &ctx.handlers,
                                &ctx.config,
                                &ctx.rate_limiters,
                            ).await {
                                tracing::error!("Error processing WebSocket message: {:?}", e);
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            tracing::error!("WebSocket error: {:?}", e);
                            break;
                        }
                        None => {
                            tracing::info!("WebSocket stream ended for {}", connection_id);
                            break;
                        }
                    }
                }

                // Handle internal messages (for sending to WebSocket)
                internal_msg = message_receiver.recv() => {
                    match internal_msg {
                        Some(msg) => {
                            if let Err(e) = timeout(Duration::from_secs(5), ws_sender.send(msg)).await {
                                tracing::error!("Failed to send WebSocket message: {:?}", e);
                                break;
                            }
                        }
                        None => {
                            tracing::info!("Internal message channel closed for {}", connection_id);
                            break;
                        }
                    }
                }
            }
        }

        // Cleanup connection
        Self::close_connection(
            connection_id,
            connections,
            senders,
            ctx.rate_limiters.clone(),
            &ctx.room_manager,
        ).await;
        ctx.connection_count.fetch_sub(1, Ordering::Relaxed);
        tracing::info!("WebSocket connection {} closed", connection_id);

        Ok(())
    }

    /// Process incoming WebSocket message
    async fn process_websocket_message(
        message: Message,
        connection_id: ConnectionId,
        connection_state: Arc<RwLock<ConnectionState>>,
        handlers: &DashMap<MessageType, Box<dyn MessageHandler + Send + Sync>>,
        config: &WebSocketConfig,
        rate_limiters: &Arc<DashMap<ConnectionId, Arc<RateLimiter>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut state = connection_state.write().await;
        state.stats.messages_received.fetch_add(1, Ordering::Relaxed);

        // Check rate limit
        if let Some(limiter) = rate_limiters.get(&connection_id) {
            if !limiter.check().await {
                tracing::warn!("Rate limit exceeded for connection {}", connection_id);
                return Err("Rate limit exceeded".into());
            }
        }

        match message {
            Message::Text(text) => {
                // Check message size
                if text.len() > config.max_message_size {
                    tracing::warn!("Message too large: {} bytes (max: {})", text.len(), config.max_message_size);
                    return Err("Message too large".into());
                }

                // Parse JSON message
                match serde_json::from_str::<WebSocketMessage>(&text) {
                    Ok(ws_msg) => {
                        state.stats.bytes_received.fetch_add(text.len(), Ordering::Relaxed);

                        // Handle message with registered handler
                        if let Some(handler) = handlers.get(&ws_msg.message_type) {
                            drop(state); // Release lock before async call
                            if let Err(e) = handler.handle(ws_msg, connection_id, &Self::dummy_manager()) {
                                tracing::error!("Message handler error: {:?}", e);
                            }
                        } else {
                            tracing::warn!("No handler registered for message type: {:?}", ws_msg.message_type);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse WebSocket message: {:?}", e);
                    }
                }
            }
            Message::Binary(data) => {
                // Check binary message size
                if data.len() > config.max_message_size {
                    tracing::warn!("Binary message too large: {} bytes (max: {})", data.len(), config.max_message_size);
                    return Err("Message too large".into());
                }
                state.stats.bytes_received.fetch_add(data.len(), Ordering::Relaxed);
                tracing::debug!("Received binary message of {} bytes", data.len());
            }
            Message::Ping(_) => {
                state.update_ping();
            }
            Message::Pong(_) => {
                state.update_pong();
            }
            Message::Close(_) => {
                tracing::info!("Received close message from {}", connection_id);
                return Err("Connection closed by client".into());
            }
        }

        Ok(())
    }

    /// Send message to a specific connection
    pub async fn send_message_to_connection(
        &self,
        connection_id: ConnectionId,
        message: WebSocketMessage,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let json_message = serde_json::to_string(&message)?;
        let ws_message = Message::Text(json_message.into());

        if let Some(sender) = self.senders.get(&connection_id) {
            sender.send(ws_message)?;
        } else {
            return Err(format!("Connection {} not found", connection_id).into());
        }

        Ok(())
    }

    /// Broadcast message to all connections
    pub async fn broadcast_message(&self, message: WebSocketMessage) -> Result<usize, Box<dyn std::error::Error>> {
        let json_message = serde_json::to_string(&message)?;
        let ws_message = Message::Text(json_message.into());
        let mut sent_count = 0;

        for sender in self.senders.iter() {
            if sender.send(ws_message.clone()).is_ok() {
                sent_count += 1;
            }
        }

        Ok(sent_count)
    }

    /// Broadcast message to a specific room
    pub async fn broadcast_to_room(
        &self,
        room_id: &RoomId,
        message: WebSocketMessage,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        // Get all connections in the room
        let room_connections = self.room_manager.get_room_connections(room_id).await;

        if room_connections.is_empty() {
            tracing::debug!("No connections in room {}", room_id);
            return Ok(0);
        }

        // Serialize message once for efficiency
        let json_message = serde_json::to_string(&message)?;
        let ws_message = Message::Text(json_message.into());
        let mut sent_count = 0;

        // Send to all connections in the room
        for connection_id in room_connections {
            if let Some(sender) = self.senders.get(&connection_id) {
                if sender.send(ws_message.clone()).is_ok() {
                    sent_count += 1;
                }
            }
        }

        tracing::debug!("Broadcast to room {} sent to {} connections", room_id, sent_count);
        Ok(sent_count)
    }

    /// Get connection state
    pub async fn get_connection_state(&self, connection_id: &ConnectionId) -> Option<Arc<RwLock<ConnectionState>>> {
        self.connections.get(connection_id).map(|entry| entry.clone())
    }

    /// Get all connection IDs
    pub fn get_connection_ids(&self) -> Vec<ConnectionId> {
        self.connections.iter().map(|entry| *entry.key()).collect()
    }

    /// Get current connection count
    pub fn get_connection_count(&self) -> usize {
        self.connection_count.load(Ordering::Relaxed)
    }

    /// Register message handler
    pub fn register_message_handler<H>(&self, message_type: MessageType, handler: H)
    where
        H: MessageHandler + 'static,
    {
        self.message_handlers.insert(message_type, Box::new(handler));
    }

    /// Register message handler (raw version for internal use)
    pub fn register_message_handler_raw(&self, message_type: MessageType, handler: Arc<dyn MessageHandler + Send + Sync>) {
        let wrapper = MessageHandlerWrapper { handler };
        self.message_handlers.insert(message_type, Box::new(wrapper));
    }

    /// Close a specific connection
    async fn close_connection(
        connection_id: ConnectionId,
        connections: Arc<DashMap<ConnectionId, Arc<RwLock<ConnectionState>>>>,
        senders: Arc<DashMap<ConnectionId, mpsc::UnboundedSender<Message>>>,
        rate_limiters: Arc<DashMap<ConnectionId, Arc<RateLimiter>>>,
        room_manager: &RoomManager,
    ) {
        connections.remove(&connection_id);
        senders.remove(&connection_id);
        rate_limiters.remove(&connection_id);
        room_manager.leave_all_rooms(&connection_id).await;
    }

    /// Create a dummy manager for testing
    fn dummy_manager() -> WebSocketManager {
        WebSocketManager::new(WebSocketConfig::default())
    }
}

impl Default for WebSocketManager {
    fn default() -> Self {
        Self::new(WebSocketConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_creation() {
        let connection_id = Uuid::new_v4();
        let addr = "127.0.0.1:8080".parse().unwrap();
        let state = ConnectionState::new(connection_id, addr);

        assert_eq!(state.id, connection_id);
        assert_eq!(state.addr, addr);
        assert!(state.rooms.is_empty());
        assert!(state.user_data.is_empty());
    }

    #[test]
    fn test_connection_stats() {
        let connection_id = Uuid::new_v4();
        let addr = "127.0.0.1:8080".parse().unwrap();
        let state = ConnectionState::new(connection_id, addr);

        state.stats.messages_sent.fetch_add(5, Ordering::Relaxed);
        state.stats.messages_received.fetch_add(3, Ordering::Relaxed);
        state.stats.bytes_sent.fetch_add(100, Ordering::Relaxed);
        state.stats.bytes_received.fetch_add(200, Ordering::Relaxed);

        assert_eq!(state.stats.messages_sent.load(Ordering::Relaxed), 5);
        assert_eq!(state.stats.messages_received.load(Ordering::Relaxed), 3);
        assert_eq!(state.stats.bytes_sent.load(Ordering::Relaxed), 100);
        assert_eq!(state.stats.bytes_received.load(Ordering::Relaxed), 200);
    }

    #[tokio::test]
    async fn test_websocket_manager_creation() {
        let config = WebSocketConfig::default();
        let manager = WebSocketManager::new(config);

        assert_eq!(manager.get_connection_count(), 0);
        assert!(manager.get_connection_ids().is_empty());
    }
}
