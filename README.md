# 跨机中转 (kzj-transfer)

局域网文件/文本/链接互传 App —— Tauri v2 桌面应用(Windows / macOS),基于
[LocalSend](https://github.com/localsend/localsend) 协议核心二次开发。

## 功能

- **聊天式 UI**:类微信,顶部是发现的设备 chip,中间是会话气泡(文本/文件/链接都混在一起按时间排),底部输入框。
- **统一输入框**:直接粘贴文本或 URL 按 Enter 发送,Shift+Enter 换行;📎 按钮发送文件。
- **两种浏览模式**:按设备 / 按文件类型分组。
- **手动添加设备**:输 IP 直连(解决跨子网/组播不通)。
- **小窗模式**:一键收纳成顶部灵动岛样式的悬浮条,继续接收。
- **双向收/发**:对接 LocalSend v2 协议,可与官方 App、CLI、其他客户端互传。

## 仓库结构

```
kzj-transfer/
├── app/                  # LocalSend 上游源码(独立 clone,不入库;见下)
│   └── packages/core     # localsend Rust crate,backend 通过 path 依赖引用
├── 跨机中转-app/          # 这个 Tauri 应用
│   ├── src-tauri/        # Rust 后端:发现/收发/配对/事件桥
│   └── ui/               # 无打包器的 HTML/CSS/JS (withGlobalTauri)
├── README.md             # 本文件:构建说明
└── 使用说明.md           # 软件使用说明
```

## 构建(通用步骤)

`app/` 是上游 LocalSend 仓库,不收进本仓库版本控制,构建前单独 clone
(与 `跨机中转-app/` 同级,目录名必须是 `app`):

```bash
git clone https://github.com/pan185618-cloud/kzj-transfer.git
cd kzj-transfer
git clone https://github.com/localsend/localsend.git app
cd 跨机中转-app
npm install
npm run tauri build
```

`src-tauri/Cargo.toml` 通过 `path = "../../app/packages/core"` 引用核心库,
目录结构必须保持 `跨机中转-app/` 与 `app/` 互为兄弟目录。

产物在 `跨机中转-app/src-tauri/target/release/`:

- Windows:`bundle\nsis\跨机中转_0.1.0_x64-setup.exe`(推荐分发)、
  `bundle\msi\跨机中转_0.1.0_x64_zh-CN.msi`、`kuajizhongzhuan.exe`(免安装)
- macOS:`bundle/dmg/`(拖入 Applications 即用)

### Windows 前置环境(一次性)

1. **Visual Studio Build Tools(MSVC,必需)** —
   `winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`
   (必须勾选「使用 C++ 的桌面开发」,提供 link.exe 和 Windows SDK)
2. **Rust(MSVC 工具链)** — `winget install --id Rustlang.Rustup`
3. **Node.js LTS** — `winget install --id OpenJS.NodeJS.LTS`

首次编译几百个 crate 约 10–25 分钟;`npx tauri build` 中途的 crate 下载慢可配
cargo 镜像源。

### macOS 前置环境

Xcode Command Line Tools(`xcode-select --install`)+ Rust + Node.js。
图标调整用到系统自带 `iconutil`,ImageMagick 可选。

## 首次运行注意

- **Windows 防火墙弹窗务必「允许访问」(勾选专用网络)**:局域网发现靠 UDP
  组播,入站传输靠 TCP 53317,被拦就互相搜不到设备。
- 接收文件默认存到「下载」目录,可在 ⚙ 设置里改。
- 两台设备须在同一局域网(Wi-Fi / 热点);搜不到时用「+ IP」手动添加对方 IP。

## 开发模式

```bash
cd 跨机中转-app
npm run tauri dev
```

详见 `跨机中转-app/打包-Windows.md`(Windows 构建细节与排错)。
