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

## 已知限制（手机上）

- 文件选择走 SAF，系统返回 `content://` URI —— tauri-plugin-dialog
  的 Android 实现会尽量转成真实路径；转不出的按字节流发送
  （后端 `FileSource::Bytes` 已支持），但目录选取不可用。
- 接收目录默认应用私有 Downloads 目录；「打开接收目录」依赖系统
  文件管理器响应 `open` intent。
- 无窗口装饰/置顶 API，灵动岛小窗模式已隐藏。
- 拖拽发送不存在（Android 无此交互）。
