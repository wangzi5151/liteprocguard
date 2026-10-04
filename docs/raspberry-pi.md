# 树莓派专用调优指南

树莓派（以及大多数老 ARM 板）的典型问题是：**散热差、容易降频、内存小**。LiteProcGuard 的目标是让后台任务“慢一点、稳一点”，而不是把进程杀掉导致服务中断。

## 1. 选择正确的产物

| 设备 | 建议产物 |
| --- | --- |
| Raspberry Pi 5 / 4 / 3（64 位系统） | `liteprocguard-linux-arm64.tar.gz` |
| Raspberry Pi 2 / Zero 2 W | `liteprocguard-linux-armv7.tar.gz` |
| Raspberry Pi 1 / Zero (armv6) | `liteprocguard-raspi-armv6.tar.gz` |

静态链接（musl），不依赖系统 glibc 版本，解压即可运行。

## 2. 先用 cgroup 还是信号回退？

```bash
./liteprocguard status
```

- 显示 `cgroup v2` / `cgroup v1`：可以用 `sudo` 获得**精确硬限制**；
- 显示 `信号回退`：普通用户或内核未开放 cgroup，程序会使用 SIGSTOP/SIGCONT 占空比限速（仍不是杀进程）。

## 3. 常见场景配置

### 3.1 后台下载 / 同步限速（不中断）

```bash
./liteprocguard rule new "transmission*" --name 下载限速 --cpu 20 --priority idle
./liteprocguard rule new "rsync"         --name 同步限速 --cpu 15 --priority idle
sudo ./liteprocguard guard start -i 2
```

### 3.2 内存保护（小内存机型）

```bash
# 超过 512 MiB 只告警，不杀
./liteprocguard rule new "node" --name node内存告警 --mem 512 --mem-action warn
# 超过 1 GiB 温和终止
./liteprocguard rule new "*python*" --mem 1024 --mem-action terminate
```

### 3.3 编译控温保护（推荐）

```bash
./liteprocguard preset apply 编译温控保护
sudo ./liteprocguard guard start -i 1 --temperature --ruleset 编译温控保护
```

当温度达到 `threshold_c`（默认 75–85°C）时，命中进程的 CPU 上限会自动进一步压低，等温度回落再恢复。

## 4. 读取温度

```bash
./liteprocguard temp
```

- 能读到：温度联动可用；
- 读不到：某些发行版 / 容器未挂载 `thermal` 或 `hwmon`，功能自动隐藏，不影响其它功能。

手动确认传感器：

```bash
cat /sys/class/thermal/thermal_zone0/temp   # 单位毫度，除以 1000
```

## 5. 开机自启（可选，由你决定）

本工具**不会自动注入自启**。如需常驻，请自行创建 systemd 服务：

```bash
sudo tee /etc/systemd/system/liteprocguard.service >/dev/null <<'EOF'
[Unit]
Description=LiteProcGuard resource guard
After=multi-user.target
[Service]
ExecStart=/usr/local/bin/liteprocguard guard start -i 2 --temperature
Restart=no
Nice=5
[Install]
WantedBy=multi-user.target
EOF

sudo install -m755 ./liteprocguard /usr/local/bin/liteprocguard
sudo systemctl daemon-reload
sudo systemctl enable --now liteprocguard
```

停止并释放限制：

```bash
sudo systemctl stop liteprocguard
# 或任何时候：
sudo liteprocguard clear
```

## 6. 低配机型建议

- 轮询间隔取 `2` 秒或更大，减少自身开销；
- 规则数量控制在个位数量级；
- 优先使用 cgroup 后端，信号回退仅作为兜底；
- 本体空闲占用目标：CPU < 0.5%、内存 < 15 MB。
