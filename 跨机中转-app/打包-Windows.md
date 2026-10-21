# 在 Windows(荣耀 MagicBook)上构建 跨机中转

## 0. 要拷过去的目录(从这台 Mac 拷贝)

只需要源码,不用带 `target/` 和 `node_modules/`:

```
跨机中转-app/
  src-tauri/     ← Cargo.toml、src/、tauri.conf.json、capabilities/、icons/
  ui/            ← index.html、style.css、app.js
  package.json、package-lock.json
app/packages/core/   ← localsend 核心库(Rust,路径依赖必须带)
```

## 1. 装构建环境(每台 Windows 机器一次)

1. **Visual Studio Build Tools** — https://visualstudio.microsoft.com/zh-hans/downloads/
   下载 "生成工具" → 安装时勾选 **"使用 C++ 的桌面开发"**(必须,提供 MSVC 链接器和 Windows SDK)。

2. **Rust** — https://rustup.rs/
   下载 `rustup-init.exe` 运行,默认安装(选 `stable-x86_64-pc-windows-msvc`)。

3. **Node.js LTS** — https://nodejs.org/zh-cn
   装完确认 `node -v`、`npm -v` 能用。

## 2. 构建

把 `跨机中转-app` 目录打开到 PowerShell / CMD:

```powershell
cd 跨机中转-app
npm install
npx tauri build
```

## 3. 产物

成功后在 `src-tauri/target/release/bundle/` 下:

- `nsis/跨机中转-0.1.0-setup.exe` — 安装包(推荐分发)
- `msi/跨机中转-0.1.0-x64.msi` — MSI 安装包
- `跨机中转.exe` — 免安装单文件,可直接拷走运行

## 4. Windows 上的「收纳」行为

工具栏「收纳」按钮照样有效:窗口会变成 320×36 的置顶小胶囊,
**停靠在屏幕顶部居中**(Windows 没有刘海,就是贴在屏幕顶边的一条小横条)。
双击胶囊或点 ⤢ 按钮还原。

代码里 `enterCompact`/`exitCompact` 不做平台判断,Tauri 在 Windows 上
同样支持无边框、置顶、跳任务栏,不需要改。

## 5. 首次运行注意

- Windows 防火墙会弹窗,**允许"专用网络"**(局域网传输依赖组播和入站 TCP/UDP)。
- 接收目录默认 `下载/`(系统目录),可在设置里改。
- 若另一台 Mac 也发现不了,检查两边是否同一 Wi-Fi / 热点,或手动在另一台里填本机 IP。

## 6. 和 Mac 版互通

两台都跑同一套 localsend 协议 v2,功能一致:自动发现、文件/文本互发、配对自动接收。
