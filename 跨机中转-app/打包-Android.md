# Android 打包

一次性的环境准备：

```bash
# 1. Rust 工具链(rustup,本机 Homebrew 的 rust 不带 android target)
brew install rustup
~/.cargo/bin/rustup-init -y --default-toolchain stable --profile minimal
ln -sf ~/.rustup/toolchains/stable-*/bin/{cargo,rustc} ~/.cargo/bin/

# 2. Android target
rustup target add aarch64-linux-android armv7-linux-androideabi \
                  i686-linux-android x86_64-linux-android

# 3. Android SDK + NDK(本机用 Homebrew 的 android-commandlinetools)
sdkmanager "ndk;27.2.12479018" "cmake;3.22.1"
```

每次构建在 `跨机中转-app/` 下执行：

```bash
export PATH="$HOME/.cargo/bin:/opt/homebrew/share/android-commandlinetools/ndk/27.2.12479018/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH"
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
export ANDROID_SDK_ROOT=$ANDROID_HOME
# 注意:不要 export NDK_HOME — Tauri 会拒绝显式设置的 NDK_HOME,
# 让它自己从 ANDROID_HOME/ndk/ 里找。

# 调试包(所有 ABI,体积大 ~900MB):
npx tauri android build --apk --debug
# 产物: src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk

# 只出 arm64(OPPO 手机都是 arm64):
npx tauri android build --apk --debug --target aarch64

# 正式包(需要签名):
npx tauri android build --apk
```

装到手机:`adb install -r <apk>`。

## 已做的适配

- `src-tauri/Cargo.toml`：`[lib] crate-type = ["staticlib","cdylib","rlib"]`;
  localsend 特性从 `full` 减到 `crypto, discovery, http, multicast`
  （webrtc 不参与桌面/手机端的局域网传输，且在 Android 上构建困难）。
- `AndroidManifest.xml`:INTERNET、ACCESS_NETWORK_STATE、ACCESS_WIFI_STATE、
  CHANGE_WIFI_MULTICAST_STATE。
- `MainActivity.kt`：持有 `WifiManager.MulticastLock`（不拿组播锁，
  Android 会丢 UDP 组播包，LocalSend 发现不到设备）。
- `backend.rs`:Android 上 `dirs::config_dir()/download_dir()` 为空，
  经 `app.path()` 取应用私有目录；`APP_HANDLE` 静态句柄在 `start()` 里设置。
- `ui/app.js`:`IS_ANDROID` 检测；隐藏「收纳到小窗」「发文件夹」按钮；
  Enter 发送逻辑不拦截手机软键盘。
- `ui/style.css`：加 `@media (max-width: 560px)` 窄屏样式。
- `package.json`：加 `"tauri": "tauri"` script(Gradle 的 rustBuild 任务
  通过 `npm run -- tauri android android-studio-script` 调 CLI)。

## 0.2.1 手机文件预览与更新

- 已发送文件保留 SAF `content://` URI，并尽可能持久化读取授权；接收文件使用本机保存路径。
- 图片由 Android 系统解码并采样为缩略图，支持系统可解码的 PNG、JPEG、HEIC 等格式；点文件卡片使用系统应用打开，并授予临时读取权限。
- 旧版本发出的历史记录没有保存 URI，升级不能补回缺失的位置；这些记录需要重新选取发送才能预览。
- 更新检查使用 Android 系统 HTTPS，依次尝试 Fastly、Gcore、jsDelivr 主域和 GitHub Raw。检查失败后可重试，详细原因留在日志和状态提示的 title 中。
- 界面回归：从仓库根目录运行 `python3 -m http.server`，打开 `/跨机中转-app/tests/message-scroll.html`；使用独立模拟历史，不访问传输后端。

## 已知限制（手机上）

- 文件和目录选择均走自带 SAF 选择器；URI 授权被来源撤销或原文件删除后，需要重新选择。PDF、Office 等文件的打开取决于手机是否安装相应应用。
- 接收目录默认应用私有 Downloads 目录；「打开接收目录」依赖系统
  文件管理器响应 `open` intent。
- 无窗口装饰/置顶 API，灵动岛小窗模式已隐藏。
- 拖拽发送不存在（Android 无此交互）。
