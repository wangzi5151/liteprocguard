# LiteProcGuard vX.Y.Z

> 发布日期：YYYY-MM-DD
> 本工具为**纯本地**进程资源管理器：无遥测、无自动更新、无任何外发网络请求。

## 亮点

- （一句话概括本次发布的核心价值）

## 新增

- [ ] 功能点 1
- [ ] 功能点 2

## 改进

- [ ] 改进点 1

## 修复

- [ ] 修复点 1

## 平台产物

| 平台 | 架构 | 文件 | 说明 |
| --- | --- | --- | --- |
| Windows | amd64 | `liteprocguard-windows-amd64.tar.gz` | 解压后运行 `.exe` |
| Windows | 386 | `liteprocguard-windows-386.tar.gz` | 32 位系统 |
| Linux | amd64 | `liteprocguard-linux-amd64.tar.gz` | 静态链接 |
| Linux | arm64 | `liteprocguard-linux-arm64.tar.gz` | 树莓派 3/4/5 64 位 |
| Linux | armv7 | `liteprocguard-linux-armv7.tar.gz` | 树莓派 2/Zero 2 W |
| Linux | armv6 | `liteprocguard-raspi-armv6.tar.gz` | 树莓派 1 / Zero |

## 校验

```bash
sha256sum -c SHA256SUMS
```

## 快速开始

```bash
# 解压后
./liteprocguard status
sudo ./liteprocguard guard start -i 2
```

## 已知限制

- CPU 硬限制在 Linux 依赖 cgroup（通常需要 root），无权限时自动回退到信号限速。
- Windows 下进程一旦加入 Job Object 无法移出，删除规则时通过关闭 CPU 速率控制来“解除限制”。
- 部分系统保护进程无法被读取或限制，已内置黑名单默认保护。

## 免责声明

本工具是用户态资源管理器，不是内核驱动。部分进程/系统服务受系统保护无法限制；请在理解风险的前提下使用，避免对关键进程施加限制导致系统异常。禁止用于恶意管控他人程序。
