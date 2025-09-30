# 心跳服务器 (Heartbeat Server)

## 项目简介

这是一个用 Rust 编写的 WebSocket 心跳监控服务器，用于监控客户端的连接状态。当客户端在指定时间内没有发送心跳消息时，服务器会向 ntfy 服务器发送超时通知。

## 功能特性

- WebSocket 连接管理
- 心跳监控和超时检测
- 支持多频道监控
- 与 ntfy 通知服务器集成
- 客户端连接断开检测
- 可配置的超时和监控间隔

## 配置选项

- `--host` / `-H`: 服务器主机地址 (默认: 0.0.0.0)
- `--port` / `-p`: 服务器端口 (默认: 14999)
- `--timeout` / `-t`: 心跳超时时间（秒）(默认: 30)
- `--monitor-interval` / `-m`: 监控检查间隔（秒）(默认: 10)
- `--max-connections` / `-c`: 最大并发连接数 (默认: 3)
- `--ntfy-addr` / `-n`: ntfy 服务器地址 (默认: http://chan.local:15000)

## 使用方法

### 运行服务器

```bash
cargo run -- [选项]
```

例如：
```bash
cargo run -- --port 15000 --timeout 60
```

### Python 客户端示例

项目包含一个 Python 客户端示例 (`examples/client.py`)，用于演示如何连接到心跳服务器：

```bash
pip install websockets pydantic loguru orjson
python examples/client.py
```

### 客户端协议

客户端通过 WebSocket 连接到服务器，并发送 JSON 格式的消息：

1. 注册消息（包含超时消息）：
```json
{
  "channel": "channel_name",
  "timeout_message": "自定义超时消息"
}
```

2. 心跳消息：
```json
{
  "channel": "channel_name"
}
```

## 依赖库

- `tokio`: 异步运行时
- `tungstenite`: WebSocket 实现
- `serde`: JSON 序列化/反序列化
- `clap`: 命令行参数解析
- `reqwest`: HTTP 客户端（用于 ntfy 通知）

## 工作原理

1. 客户端通过 WebSocket 连接到服务器
2. 服务器记录每个频道的最后心跳时间
3. 监控任务定期检查各频道的超时状态
4. 当频道超时时，向 ntfy 服务器发送通知

## 通知系统

服务器与 ntfy 通知系统集成，当检测到心跳超时或客户端断开连接时，会自动发送通知到指定的频道。

---

*本项目由 Qwen 生成*