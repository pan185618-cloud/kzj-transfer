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
- 系统截图浮窗能否发起跨应用拖放取决于手机系统；不支持拖放时可使用截图提示条或系统分享。

## 0.2.2 截图快捷发送

- 输入框上方提供「开启」入口；也可在设置里开启或关闭「截图快捷发送」。开启时才请求照片权限，不申请视频权限。
- 开启后，在 App 前台或切回 App 时查询最近三分钟的截图，展示缩略图和接收设备，点击「发送」才传输；忽略或已发送的截图不再提示，过期自动收起。附件菜单也提供「最近截图」。
- Android 14 及以上支持全部、部分照片或拒绝授权。部分授权只能查询已选照片，因此新截图可能不提示；设置显示权限状态，可重新授权。拒绝照片权限也能使用系统拖入和分享入口。
- WebView 原生接收 `DragEvent`，在放下时取得临时 URI 读取授权；系统「分享」支持单张、多张 `image/*`。冷启动、已运行的 `singleTask` 均能处理分享。
- 拖入、分享的图片先复制到应用私有 `files/quick-send/`，并持久保存待发送队列，避免浮窗/分享来源释放授权后图片失效。没有接收设备时保留队列，选择在线设备后发送；当前会话内队列确认失败只重试确认，不重复启动传输。副本用于发送历史预览，清空聊天记录不会删除它们。
- 仅在前台注册 MediaStore 变更监听，无后台常驻服务。截图建议依赖系统媒体库的截图文件名或相册路径；未保存到公共相册的截图可直接使用系统分享。
- 前端回归：`tests/android-quick-send.html` 检查提示、权限、忽略、过期、防重复、离线队列、批量分享、失败重试和拖入高亮；原有 `tests/message-scroll.html` 继续检查滚动与更新功能。
- Android 参考：[媒体库权限](https://developer.android.com/training/data-storage/shared/media)、[部分照片授权](https://developer.android.com/about/versions/14/changes/partial-photo-video-access)、[跨应用拖放授权](https://developer.android.com/reference/android/view/DragAndDropPermissions)、[接收系统分享](https://developer.android.com/develop/ui/compose/sharing/receive)。
- 2026-11-07 验证：Android arm64 正式构建及签名校验通过，桌面 Rust `cargo check --locked --offline` 通过；两个 WebKit 回归页面共 25 项全部通过。Android 13 模拟器验证截图提示、运行中系统分享和离线队列重启保留；OPPO Find X8 Pro 真机验证截图提示一键发送、左侧浮窗长按拖入并发送到 Mac，以及发送后收起同图建议。已覆盖安装 0.2.2，保留原数据。
