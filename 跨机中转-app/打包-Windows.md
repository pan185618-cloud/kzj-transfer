# 在 Windows 上构建 跨机中转

Mac 端与 Windows 端共用同一套源码(本仓库),在 Windows 上按以下步骤即可编译。

## 0. 取源码(两条 clone)

```powershell
git clone https://github.com/pan185618-cloud/kzj-transfer.git
cd kzj-transfer
git clone https://github.com/localsend/localsend.git app
```

`app/` 是上游 LocalSend 仓库(不入库),必须与 `跨机中转-app/` 同级、
目录名必须是 `app`——`src-tauri/Cargo.toml` 通过
`path = "../../app/packages/core"` 引用核心库。

## 1. 装构建环境(每台 Windows 机器一次)

1. **Visual Studio Build Tools** — https://visualstudio.microsoft.com/zh-hans/downloads/
   下载 "生成工具" → 安装时勾选 **"使用 C++ 的桌面开发"**(必须,提供 MSVC 链接器和 Windows SDK)。
   或 winget 一条命令:
   ```powershell
   winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```

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

首次要编译几百个 Rust crate,正常需要 10–25 分钟,不要中途关窗口。

## 3. 产物

成功后在 `src-tauri/target/release/` 下:

- `bundle\nsis\跨机中转_0.1.0_x64-setup.exe` — 安装包(推荐分发)
- `bundle\msi\跨机中转_0.1.0_x64_zh-CN.msi` — MSI 安装包
- `kuajizhongzhuan.exe` — 免安装单文件,可直接拷走运行

> MSI 的 WiX 语言已配置为 `zh-CN`(见 `tauri.conf.json` → `bundle.windows.wix.language`),
> 否则中文产品名会报 `LGHT0311 ... code page '1252'` 打包失败。

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

## 6. 排错

| 报错 | 原因 / 解决 |
|---|---|
| `link.exe` / `LNK` 错误 | 没装 MSVC 生成工具 → 回到第 1 步 |
| `rustc: 不是内部或外部命令` | rustup 装了但没重开终端 → 重开 PowerShell |
| `failed to read ... app/packages/core` | 目录结构变了 → 保证 `跨机中转-app` 和 `app` 是兄弟目录 |
| 卡在下载 crate 很久 | 网络问题;可设置国内镜像(`$env:HTTP_PROXY=...` 或改 cargo 镜像源) |
| NSIS / WiX 下载失败 | 一般是网络问题,重跑一次 `npx tauri build` |
| `LGHT0311 ... code page` | MSI 语言不是 zh-CN → 确认 `tauri.conf.json` 里 `wix.language = "zh-CN"` |
| 设备互相搜不到 | 防火墙没放行专用网络;或不在同一 Wi-Fi;或用「+ IP」手动添加 |

## 7. 和 Mac 版互通

两台都跑同一套 localsend 协议 v2,功能一致:自动发现、文件/文本互发、配对自动接收。
