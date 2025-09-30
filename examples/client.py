import asyncio
import orjson
import websockets
from pydantic import BaseModel
import time
import threading
from loguru import logger


class HeartBeatConfig(BaseModel):
    """客户端配置"""
    server_host: str = "chan.local"
    server_port: int = 14999
    heartbeat_interval: int = 60  # 秒
    channel_id: str = "gp_factory"
    timeout_message: str = "gp_factory可能已停止运行(客户端: {client_address}) "


class HeartbeatClient:
    def __init__(self, config: HeartBeatConfig | None = None):
        self.config = config or HeartBeatConfig()
        self.server_host = self.config.server_host
        self.server_port = self.config.server_port
        self.channel_id = self.config.channel_id
        self.heartbeat_interval = self.config.heartbeat_interval
        self.running = False
        # websocket 仅在心跳线程内创建并管理，不在主线程共享
        self._thread: threading.Thread | None = None

    async def _heartbeat_loop(self):
        websocket_uri = f"ws://{self.server_host}:{self.server_port}"
        ws = None
        try:
            ws = await websockets.connect(websocket_uri)
            logger.success(
                f"已连接到心跳服务器 {self.server_host}:{self.server_port}，订阅: {self.channel_id}, 心跳间隔: {self.heartbeat_interval} 秒"
            )

            # 连接后立即发送一次注册/超时消息（registration）
            registration_data = {
                "channel": self.channel_id,
                "timeout_message": self.config.timeout_message,
                "timestamp": time.time(),
                "type": "register",
            }
            await ws.send(orjson.dumps(registration_data))
            logger.info("已发送注册/超时消息")

            # 循环发送心跳
            while self.running:
                try:
                    heartbeat_data = {
                        "channel": self.channel_id,
                        "timestamp": time.time(),
                        "type": "heartbeat",
                    }
                    await ws.send(orjson.dumps(heartbeat_data))
                    logger.info(
                        f"已发送心跳 {self.server_host}:{self.server_port}，订阅: {self.channel_id}"
                    )
                except websockets.exceptions.ConnectionClosed as e:
                    logger.warning(f"WebSocket 连接已关闭: {e}. 停止心跳循环。")
                    break
                except Exception as e:
                    logger.error(f"发送心跳时发生错误: {e}")
                    break

                await asyncio.sleep(self.heartbeat_interval)

        except Exception as e:
            logger.error(f"心跳线程连接/运行出错: {e}")
        finally:
            if ws:
                try:
                    await ws.close()
                    logger.info("心跳线程中已断开与服务器的连接")
                except Exception as e:
                    logger.warning(f"关闭 websocket 时出错: {e}")

    def start_heartbeat(self):
        """在后台线程中启动心跳循环（使用独立的 asyncio 事件循环）"""
        if self._thread and self._thread.is_alive():
            logger.warning("心跳线程已经在运行")
            return self._thread

        self.running = True

        def run():
            try:
                asyncio.run(self._heartbeat_loop())
            except Exception as e:
                logger.error(f"心跳线程异常退出: {e}")

        self._thread = threading.Thread(target=run, daemon=True)
        self._thread.start()
        return self._thread

    def stop(self):
        """停止心跳线程（会在下一个循环或连接异常时退出并关闭 websocket）"""
        self.running = False
        if self._thread:
            self._thread.join(timeout=1)

# ---------- main ----------
def main():
    import sys
    import tomllib
    import argparse
    from pathlib import Path

    logger.info("正在启动心跳客户端...")

    parser = argparse.ArgumentParser(description="心跳客户端")
    parser.add_argument("-n", type=int, default=3, help="心跳间隔秒数 (默认: 3)")
    parser.add_argument("config_file", nargs="?", help="配置文件路径")
    args = parser.parse_args()

    if args.config_file:
        config_path = Path(args.config_file)
        if not config_path.exists():
            logger.error(f"配置文件 {config_path} 不存在")
            sys.exit(1)
        logger.info(f"配置将从 {config_path} 加载")
        with config_path.open("rb") as f:
            data = tomllib.load(f)
        # command-line override
        data["heartbeat_interval"] = args.n
        client_config = HeartBeatConfig.model_validate(data)
    else:
        logger.info("未指定配置文件，使用默认配置")
        client_config = HeartBeatConfig(
            server_host="localhost",
            server_port=14999,
            heartbeat_interval=args.n,
            channel_id="test",
            timeout_message="(客户端: {client_address}) 已经 {time_since_heartbeat:.2f} 秒没有发送心跳",
        )

    client = HeartbeatClient(client_config)
    client.start_heartbeat()

    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        logger.info("收到退出信号，正在停止心跳客户端...")
        client.stop()
        sys.exit(0)


if __name__ == "__main__":
    main()
