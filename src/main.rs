use clap::Parser;
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use reqwest;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, RwLock};
use tokio_tungstenite::{accept_async, tungstenite::Error as WsError};

#[derive(Parser)]
#[clap(name = "Heartbeat Server", about = "A heartbeat monitoring server")]
struct Args {
    /// Server host address
    #[clap(long, default_value = "0.0.0.0", short = 'H')]
    host: String,

    /// Server port
    #[clap(long, default_value = "14999", short = 'p')]
    port: u16,

    /// Heartbeat timeout in seconds
    #[clap(long, default_value = "30", short = 't')]
    timeout: u64,

    /// Monitor interval in seconds
    #[clap(long, default_value = "10", short = 'm')]
    monitor_interval: u64,

    /// Maximum concurrent connections
    #[clap(long, default_value = "3", short = 'c')]
    max_connections: usize,

    /// Ntfy server address
    #[clap(long, default_value = "http://chan.local:15000", short = 'n')]
    ntfy_addr: String,
}

#[derive(Debug, Clone)]
struct ChannelInfo {
    channel_id: String,
    last_heartbeat: f64,
    client_address: String,
    is_monitoring: bool,
    timeout_notified: bool,
    timeout_message: String,
}

impl ChannelInfo {
    fn new(channel_id: String, client_address: String, timeout_message: String) -> Self {
        ChannelInfo {
            channel_id,
            last_heartbeat: current_timestamp(),
            client_address,
            is_monitoring: true,
            timeout_notified: false,
            timeout_message,
        }
    }
}

#[derive(Deserialize)]
struct HeartbeatMessage {
    channel: Option<String>,
    timeout_message: Option<String>,
}

struct HeartbeatServer {
    host: String,
    port: u16,
    timeout: u64,
    monitor_interval: u64,
    ntfy_addr: String,
    channels: Arc<RwLock<HashMap<String, ChannelInfo>>>,
    shutdown_notify: Arc<Notify>,
}

impl HeartbeatServer {
    fn new(args: Args, shutdown_notify: Arc<Notify>) -> Self {
        HeartbeatServer {
            host: args.host,
            port: args.port,
            timeout: args.timeout,
            monitor_interval: args.monitor_interval,
            ntfy_addr: args.ntfy_addr,
            channels: Arc::new(RwLock::new(HashMap::new())),
            shutdown_notify,
        }
    }

    async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = format!("{}:{}", self.host, self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        info!("心跳服务器在 {}:{} 上启动", self.host, self.port);
        info!("心跳超时: {} 秒", self.timeout);
        info!("按 Ctrl+C 停止服务器");

        // Start monitoring task
        let channels_clone = self.channels.clone();
        let shutdown_notify_clone = self.shutdown_notify.clone();
        let timeout = self.timeout;
        let monitor_interval = self.monitor_interval;
        let ntfy_addr = self.ntfy_addr.clone();

        tokio::spawn(async move {
            monitor_channels(
                channels_clone,
                shutdown_notify_clone,
                timeout,
                monitor_interval,
                ntfy_addr,
            )
            .await;
        });

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, client_addr)) => {
                            let channels_clone = self.channels.clone();
                            let ntfy_addr = self.ntfy_addr.clone();

                            tokio::spawn(async move {
                                if let Err(e) = handle_client(stream, client_addr.to_string(), channels_clone, ntfy_addr).await {
                                    error!("处理客户端连接时发生错误: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("接受连接时发生错误: {}", e);
                        }
                    }
                }
                _ = self.shutdown_notify.notified() => {
                    info!("服务器收到关闭信号");
                    break;
                }
            }
        }

        Ok(())
    }
}

async fn handle_client(
    stream: tokio::net::TcpStream,
    client_address: String,
    channels: Arc<RwLock<HashMap<String, ChannelInfo>>>,
    ntfy_addr: String,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("新客户端从 {} 连接", client_address);

    let websocket = accept_async(stream).await?;
    let (_sender, mut receiver) = websocket.split();

    // Keep track of channels this client is associated with
    let mut client_channels = Vec::new();

    loop {
        use futures_util::StreamExt;
        if let Some(msg) = receiver.next().await {
            match msg {
                Ok(message) => {
                    if message.is_close() {
                        break;
                    }

                    if let Ok(text) = message.into_text() {
                        if let Ok(heartbeat_data) = serde_json::from_str::<HeartbeatMessage>(&text)
                        {
                            if let Some(channel_id) = heartbeat_data.channel {
                                let timeout_message =
                                    heartbeat_data.timeout_message.unwrap_or_default();

                                // Check if this is a registration message with timeout_message
                                if !timeout_message.is_empty() {
                                    update_heartbeat(
                                        &channels,
                                        &channel_id,
                                        &client_address,
                                        &timeout_message,
                                    )
                                    .await;
                                    info!(
                                        "收到客户端注册信息 - 通道: {}, 客户端: {}, 自定义超时消息: {}",
                                        channel_id, client_address, timeout_message
                                    );
                                    client_channels.push(channel_id.clone());
                                } else {
                                    // This is a regular heartbeat message
                                    update_heartbeat(&channels, &channel_id, &client_address, "")
                                        .await;
                                    info!(
                                        "从通道 {} 收到心跳 (客户端: {})",
                                        channel_id, client_address
                                    );
                                }
                            } else {
                                warn!("客户端 {} 发送的心跳消息没有通道ID", client_address);
                            }
                        } else {
                            warn!("客户端 {} 发送了无效的JSON: {}", client_address, text);
                        }
                    }
                }
                Err(e) => match e {
                    WsError::ConnectionClosed | WsError::AlreadyClosed => {
                        info!("客户端 {} 连接已关闭", client_address);
                        break;
                    }
                    _ => {
                        error!("接收消息时发生错误: {}", e);
                        break;
                    }
                },
            }
        } else {
            break;
        }
    }

    info!("客户端 {} 已断开连接", client_address);

    // Find all channels associated with this client address and mark them as disconnected
    let disconnected_channels = {
        let channels_read = channels.read().await;
        channels_read
            .iter()
            .filter(|(_, info)| info.client_address == client_address && info.is_monitoring)
            .map(|(id, info)| (id.clone(), info.clone()))
            .collect::<Vec<_>>()
    };

    // Send immediate disconnection notifications for all affected channels
    for (channel_id, mut channel_info) in disconnected_channels {
        let _timeout_message = format!("(客户端: {}) 已断开连接", channel_info.client_address);
        error!(
            "客户端连接断开 - 通道 {} (客户端: {})",
            channel_id, channel_info.client_address
        );

        // Mark as timeout notified to prevent duplicate notifications
        channel_info.timeout_notified = true;

        // Update the channel with the new status
        {
            let mut channels_write = channels.write().await;
            if let Some(info) = channels_write.get_mut(&channel_id) {
                info.timeout_notified = true;
            }
        }

        // Send immediate notification about disconnection
        send_timeout_notification(
            &ntfy_addr,
            &channel_info,
            0.0,  // time_since_heartbeat is 0 for immediate disconnection
            true, // is_disconnection
        )
        .await;
    }

    Ok(())
}

async fn update_heartbeat(
    channels: &Arc<RwLock<HashMap<String, ChannelInfo>>>,
    channel_id: &str,
    client_address: &str,
    timeout_message: &str,
) {
    let current_time = current_timestamp();
    let mut channels_write = channels.write().await;

    if let Some(channel_info) = channels_write.get_mut(channel_id) {
        // Update existing channel
        channel_info.last_heartbeat = current_time;

        // If the channel was previously marked as timeout, now it reconnects
        // Reset timeout notification status for future timeouts
        if channel_info.timeout_notified {
            info!("通道 {} 已重新连接", channel_id);
            channel_info.timeout_notified = false;
        }

        // Update timeout message if provided
        if !timeout_message.is_empty() {
            channel_info.timeout_message = timeout_message.to_string();
        }

        debug!("已更新通道 {} 的心跳", channel_id);
    } else {
        // Create new channel - this will start monitoring
        let stored_timeout_message = if timeout_message.is_empty() {
            "(客户端: {client_address}) 已经 {time_since_heartbeat:.2f} 秒没有发送心跳".to_string()
        } else {
            timeout_message.to_string()
        };

        let channel_info = ChannelInfo::new(
            channel_id.to_string(),
            client_address.to_string(),
            stored_timeout_message,
        );

        channels_write.insert(channel_id.to_string(), channel_info);
        info!("开始监控新通道: {}", channel_id);
    }
}

async fn format_timeout_message(
    channel_info: &ChannelInfo,
    time_since_heartbeat: f64,
    is_disconnection: bool,
) -> String {
    if is_disconnection {
        format!("(客户端: {}) 已断开连接", channel_info.client_address)
    } else {
        channel_info
            .timeout_message
            .replace("{client_address}", &channel_info.client_address)
            .replace(
                "{time_since_heartbeat:.2f}",
                &format!("{:.2}", time_since_heartbeat),
            )
    }
}

async fn send_timeout_notification(
    ntfy_addr: &str,
    channel_info: &ChannelInfo,
    time_since_heartbeat: f64,
    is_disconnection: bool,
) {
    let message =
        format_timeout_message(channel_info, time_since_heartbeat, is_disconnection).await;

    match send_ntfy_notification(ntfy_addr, &channel_info.channel_id, &message).await {
        Ok(_) => {
            info!(
                "通知已发送到ntfy服务器 - 地址: {}/{}",
                ntfy_addr, channel_info.channel_id
            );
        }
        Err(e) => {
            error!("发送ntfy通知失败: {}", e);
        }
    }
}

async fn send_ntfy_notification(
    ntfy_addr: &str,
    channel_id: &str,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let url = format!("{}/{}", ntfy_addr, channel_id);

    client
        .post(&url)
        .header("Priority", "urgent")
        .header("Tags", "warning")
        .body(message.to_string())
        .send()
        .await?;

    Ok(())
}

async fn monitor_channels(
    channels: Arc<RwLock<HashMap<String, ChannelInfo>>>,
    shutdown_notify: Arc<Notify>,
    timeout: u64,
    monitor_interval: u64,
    ntfy_addr: String,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(monitor_interval)) => {
                let current_time = current_timestamp();

                // Get all channels for processing
                let channels_to_check = {
                    let channels_read = channels.read().await;
                    channels_read
                        .iter()
                        .map(|(id, info)| (id.clone(), info.clone()))
                        .collect::<Vec<_>>()
                };

                for (channel_id, channel_info) in channels_to_check {
                    let time_since_heartbeat = current_time - channel_info.last_heartbeat;

                    if time_since_heartbeat > timeout as f64 && !channel_info.timeout_notified {
                        _ = format_timeout_message(&channel_info, time_since_heartbeat, false).await;
                        error!(
                            "心跳超时 - 通道 {} (客户端: {}) 已经 {:.2} 秒没有发送心跳",
                            channel_id, channel_info.client_address, time_since_heartbeat
                        );

                        send_timeout_notification(&ntfy_addr, &channel_info, time_since_heartbeat, false).await;

                        // Mark this channel as timeout notified
                        let mut channels_write = channels.write().await;
                        if let Some(info) = channels_write.get_mut(&channel_id) {
                            info.timeout_notified = true;
                        }
                    }
                }
            }
            _ = shutdown_notify.notified() => {
                info!("监控任务收到关闭信号");
                break;
            }
        }
    }
}

fn current_timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args = Args::parse();

    info!("正在启动心跳服务器...");
    info!(
        "配置 - 主机: {}, 端口: {}, 超时: {}秒",
        args.host, args.port, args.timeout
    );

    let shutdown_notify = Arc::new(Notify::new());
    let shutdown_notify_clone = shutdown_notify.clone();

    // Handle Ctrl+C to gracefully shutdown
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.expect("无法监听 Ctrl+C");
        info!("正在关闭心跳服务器...");
        shutdown_notify_clone.notify_waiters();
    });

    let mut server = HeartbeatServer::new(args, shutdown_notify);
    server.start().await?;

    Ok(())
}
