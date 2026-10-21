# 跨机中转 (kzjj-transfer)

局域网文件/文本/链接互传 App —— Tauri v2 桌面应用,基于 [LocalSend](https://github.com/localsend/localsend) 二次开发。

## 功能

- **聊天式 UI**:类微信,顶部是发现的设备 chip,中间是会话气泡(文本/文件/链接都混在一起按时间排),底部输入框。
- **统一输入框**:直接粘贴文本或 URL 按 Enter 发送,Shift+Enter 换行;📎 按钮发送文件。
- **两种浏览模式**:按设备 / 按文件类型分组。
- **手动添加设备**:输 IP 直连(解决跨子网/组播不通)。
- **小窗模式**:一键收纳成顶部灵动岛样式的悬浮条,继续接收。
- **双向收/发**:对接 LocalSend v2 协议,可与官方 App、CLI、其他客户端互传。

## 仓库结构

```
跨机中转/
├── app/                  # LocalSend 上游源码 (作为 git submodule / 独立 clone)
│   └── packages/core     # localsend Rust crate,backend 引用
├── 跨机中转-app/          # 这个 Tauri 应用
│   ├── src-tauri/        # Rust 后端:发现/收发/配对/事件桥
│   └── ui/               # 无打包器的 HTML/CSS/JS (withGlobalTauri)
└── 使用说明.md
```

## 构建前置

1. 把 [LocalSend](https://github.com/localsend/localsend) clone 到本仓库的 `app/` 目录(与 `跨机中转-app/` 同级):
   ```bash
   git clone https://github.com/localsend/localsend.git app
   ```
   **必须** 是 `app/packages/core` 这个 cargo crate 可见 —— 跨机中转-app 的 `src-tauri/Cargo.toml` 通过 `path = "../../app/packages/core"` 引用它。

2. 安装依赖:
   - Rust toolchain(`cargo`)
   - Node.js + npm(用于 `tauri` CLI)
   - macOS 上 `iconutil`(系统自带)、ImageMagick(可选,改图标用)

## 构建 / 运行

```bash
cd 跨机中转-app
npm install
npm run tauri build     # 打包 release
npm run tauri dev       # 开发模式
```

打包产物在 `跨机中转-app/src-tauri/target/release/bundle/`。

## Windows 端

`app` 目录在上游仓库是 git 仓库,所以这边没有把它收进版本控制。Windows 上同步方式:

```powershell
git clone <这个仓库的 URL> 跨机中转
cd 跨机中转
git clone https://github.com/localsend/localsend.git app
cd 跨机中转-app
npm install
npm run tauri build
```

详见 `跨机中转-app/打包-Windows.md`。
