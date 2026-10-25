# MCHOSE Battery

迈从（MCHOSE）外设电量托盘程序。Windows 原生 Rust + Win32/GDI 实现，单文件、无 Electron、无 WebView，静置时 CPU 占用为 0。

![界面预览](preview.png)

## 下载

不自行编译的话，直接到 [Releases](https://github.com/Tensyd/mchose-battery/releases) 下载 `MCHOSE-Battery.exe`，双击运行即可。

## 支持的设备

| 设备 | 连接方式 | 识别标识 |
| --- | --- | --- |
| A7 V2 Ultra+ 鼠标 | 2.4G 接收器 | `3837:100B` |
| G87 V2 键盘 | 2.4G 接收器 | `3837:201B` |
| V9 PRO 耳机 | 2.4G 接收器 | `291D:385D` |

## 功能

- 三设备电量卡片，图标 / 型号 / 百分比 / 电量条。
- 20% 及以下电量标红（百分比与进度条同时标红），其余为品牌科技蓝。
- 开机自启开关，写入当前用户启动项，无需管理员权限。
- 系统托盘常驻：悬停显示三设备电量摘要，左键恢复窗口，右键菜单显示主界面 / 退出，关闭窗口隐藏到托盘。
- 按需刷新：仅在窗口聚焦或鼠标点击时读取一次，其余时间完全休眠，无定时轮询。

## 数据状态约定

- 读取失败不显示伪造数值，直接显示 `--`。
- 键盘休眠导致读取超时时保留上次读数，但以灰色呈现；接收器移除则清空。
- 耳机未响应时不保留旧读数。
- 所有电量查询均为只读报文，不发送未经确认的写命令。

## 运行

Windows 10 / 11 x64，双击 exe 即可，无需安装运行时。

## 构建

```bash
cargo build --release
```

需要 Rust stable 与 Windows 链接环境。产物为 `target/release/mchose-battery.exe`，仅依赖系统 DLL。

## 项目结构

```
src/
  main.rs      程序入口
  device.rs    HID 枚举与三款设备协议解析
  ui.rs        窗口、绘制与交互
  tray.rs      系统托盘与悬停摘要
  autostart.rs 开机自启注册表读写
  logo.bgra    官网表头字标位图
  app-icon.bgra 程序图标位图
```

## 说明

电量数值直接来自设备报文，可能与官方软件按 10% 档位取整的显示存在差异。非本表列出的型号不会自动适配。
