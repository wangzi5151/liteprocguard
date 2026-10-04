# Termux（Android）完整上手教程

本教程面向在安卓手机上使用 Termux 折腾的用户，目标是**编译或运行后台任务时给手机降温、防止卡顿**，且不把手机越狱、不改系统。

## 1. 安装依赖

```bash
pkg update -y
pkg install -y git rust binutils
```

> 若已安装 Rust 可跳过。

## 2. 获取并编译

```bash
git clone https://github.com/wangzi5151/liteprocguard.git
cd liteprocguard
bash build-termux.sh
```

脚本会自动检查并安装 Rust，然后编译出适配当前设备的单文件：

```text
target/release/liteprocguard
```

也可以一条命令编译并直接进入菜单：

```bash
bash build-termux.sh run
```

## 3. 首次运行

```bash
./target/release/liteprocguard
```

会看到数字菜单：

```
  1 = 列出进程
  2 = 新建限速规则
  3 = 启用/停止守护
  4 = 查看日志
  5 = 退出
```

## 4. 给编译任务控温（最常用）

Termux 里安装 `clang`、`gcc`、`rust` 后编译大项目时手机会明显发热。使用内置预设：

```bash
./target/release/liteprocguard status                 # 先看后端
./target/release/liteprocguard preset apply 编译温控保护
./target/release/liteprocguard guard start -i 2 --temperature
```

- 若 `status` 显示“信号回退”，说明普通用户无法用 cgroup，程序会用占空比限速（依然不是杀进程）。
- 若温度可读，超过阈值会自动进一步压低 CPU 上限。

## 5. 限制其它进程

```bash
# 按名字限制，例如限制某个后台服务最多用 20% CPU
./target/release/liteprocguard rule new "myservice*" --name 服务限速 --cpu 20 --priority below_normal

# 内存超过 512MiB 只告警
./target/release/liteprocguard rule new "node" --mem 512 --mem-action warn
```

## 6. 停止与恢复

```bash
./target/release/liteprocguard guard stop   # 停止守护并释放限制
./target/release/liteprocguard clear        # 紧急清除全部限制
```

## 7. Web UI（可选）

```bash
./target/release/liteprocguard web --port 7317
```

打开输出的 `http://127.0.0.1:7317/?token=...` 即可在手机浏览器里用图形界面操作。

## 8. 常见问题

**Q：为什么只看到很少的进程？**
A：Android 的 SELinux 限制了跨应用读取 `/proc`，只能看到自己有权限的进程，属正常现象。

**Q：能限制其它 App 吗？**
A：普通 Termux 用户通常不能。需要 root（如通过 `su`）或配合具备权限的环境（如 Shizuku 相关方案）才有更高权限。

**Q：会常驻后台吗？**
A：**不会**。只有你显式运行 `guard start` 或 `guard bg` 才会轮询；`guard stop` / 退出即完全释放。

**Q：会不会偷偷联网？**
A：不会。程序没有任何外发网络请求，Web UI 只绑定 `127.0.0.1`。

**Q：编译失败，提示找不到链接器？**
A：先 `pkg install binutils`，必要时 `pkg install clang`；仍失败请附上完整错误提 Issue。
