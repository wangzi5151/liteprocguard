# Windows 注意事项

## 1. 选择产物

- 绝大多数机器：`liteprocguard-windows-amd64.tar.gz`
- 32 位系统 / 极老机器：`liteprocguard-windows-386.tar.gz`

解压后得到 `liteprocguard.exe`，可直接双击（进入菜单）或在 PowerShell / cmd 中运行。

## 2. 以管理员身份运行

硬 CPU 上限依赖 **Job Object**，需要管理员权限：

- 右键 `liteprocguard.exe` → “以管理员身份运行”；或
- 打开“管理员 PowerShell”，`cd` 到目录后执行：

```powershell
.\liteprocguard.exe guard start -i 2
```

普通权限下程序不会报堆栈错误，而是给出中文提示，并自动回退到信号限速。

## 3. 解除限制的方式

Windows 无法把进程从 Job Object 中移出，因此：

- 删除规则 / `guard stop` / `clear` 时，程序通过**关闭该 Job 的 CPU 速率控制**来解除限制；
- **不会终结进程**；
- 如果目标进程本来就在别的 Job 中（部分服务、容器、调试场景），加入会失败，程序会记录日志并跳过该进程。

## 4. 服务进程与黑名单

- 服务进程运行在 **Session 0**，列表里会标注“服务”；
- 默认黑名单已包含 `svchost.exe`、`lsass.exe`、`explorer.exe`、`dwm.exe` 等，避免误伤；
- 若要保护自定义关键程序，编辑规则集 JSON 的 `blacklist` 字段。

## 5. 命令行速查（PowerShell）

```powershell
.\liteprocguard.exe status
.\liteprocguard.exe list -f chrome -n 20
.\liteprocguard.exe rule new "*chrome*" --cpu 30 --mem 2048 --priority below_normal
.\liteprocguard.exe guard start -i 2 --temperature
.\liteprocguard.exe guard stop
.\liteprocguard.exe clear
.\liteprocguard.exe web --port 7317
```

## 6. 数据目录

```text
%APPDATA%\LiteProcGuard\
  config.json
  rules\
  logs\
```

## 7. Win7 兼容

程序使用标准 Win32 API（ToolHelp32、Job Object、psapi），尽量兼容旧系统。若在 Win7 上遇到问题，请附系统版本与错误信息提 Issue。
