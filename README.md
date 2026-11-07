# ClipSync 剪贴板同步

跨设备（手机 ↔ 电脑）**文本剪贴板**同步工具，使用你自己的 WebDAV 作为中转。

- 数据只经过你自己的服务器，不依赖任何第三方云服务
- 用 Rust + Tauri 2 编写，同一套代码打包 macOS 应用与 Android APK
- 支持 macOS 菜单栏常驻、Android 前台同步

## 工作原理

两端各自轮询本机剪贴板：

1. 剪贴板内容变化 → 写入 WebDAV 上的 `<同步目录>/clip-sync.json`（HTTP PUT）
2. 定时拉取该文件 → 若内容来自**另一台设备**，写入本机剪贴板
3. 用「设备标识 + 内容比对」避免自己写、自己读的循环

## 目录结构

```
clip-sync/
├── index.html / src/        # 前端界面（WebDAV 配置、状态、同步记录）
└── src-tauri/               # Rust 后端
    ├── src/lib.rs           # 同步循环、WebDAV 读写、剪贴板、托盘、命令
    ├── Cargo.toml
    └── tauri.conf.json
```

## 功能

- **文字同步**：两端剪贴板内容经 WebDAV 实时同步（默认 1.5 秒轮询）
- **图片同步**：
  - Mac → 手机：自动保存到相册 `Pictures/ClipSync`
  - 手机 → Mac：在相册/微信里选图 → **分享 → ClipSync**，上传后 Mac 端自动收到（安卓剪贴板不支持图片，故走系统分享）
- **同步记录**：最近 7 天，持久化到本地，含图片缩略图
- **收藏**：永久保留，并随 WebDAV 在多端合并同步（删除用墓碑记录，避免回流）
- **一键清理**：清空相册/缓存中的图片
- **macOS 菜单栏**：显示最新一条同步记录
- **全局快捷键**：默认 `⌘⇧V` 呼出窗口，可在设置中自定义
- **键盘操作**：`↑↓` 选择记录、`←→` 切换「最近/收藏」、`Enter` 直接粘贴到光标处、`Esc` 隐藏

## 构建

### 通用

```bash
npm install
```

### macOS

```bash
npm run tauri build
# 产物：src-tauri/target/release/bundle/macos/ClipSync.app
```

### macOS 签名（重要）

macOS 的「辅助功能」授权（`Enter` 直接粘贴需要）绑定 App 的签名身份。若每次打包都用 ad-hoc 签名（`codesign -s -`），签名哈希会变，授权就会失效、需要重新勾选。

解决办法：创建一次固定的自签名证书，之后所有版本都用它签名。

```bash
# 1. 生成证书（仅一次）
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem -days 3650 -nodes \
  -subj "/CN=ClipSync Dev" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "basicConstraints=critical,CA:false"

openssl pkcs12 -export -legacy -inkey key.pem -in cert.pem -out id.p12 \
  -passout pass:clipsync -name "ClipSync Dev"

security import id.p12 -k "$HOME/Library/Keychains/login.keychain-db" \
  -P clipsync -T /usr/bin/codesign -A
security add-trusted-cert -r trustRoot \
  -k "$HOME/Library/Keychains/login.keychain-db" cert.pem

security find-identity -v -p codesigning   # 应能看到 "ClipSync Dev"
```

之后用脚本构建部署，会自动用该证书签名：

```bash
./scripts/deploy-macos.sh
```

首次运行后，到「系统设置 → 隐私与安全性 → 辅助功能」勾选 `ClipSync.app` 即可，以后升级无需重新授权（除非换证书）。

### Android

前置：JDK 17+、Android SDK、NDK。

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
tauri android init
tauri android build --apk
# 产物：src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

> 中国大陆网络环境提示：
> - rustup 组件下载失败时，可临时指定官方源 `RUSTUP_DIST_SERVER=https://static.rust-lang.org`
> - Gradle 拉取 `dl.google.com` / `repo.maven.apache.org` 出现 TLS 握手失败时，在 `src-tauri/gen/android/build.gradle.kts` 与 `buildSrc/build.gradle.kts` 的 `repositories` 中优先加入阿里云镜像：
>   `https://maven.aliyun.com/repository/public`、`/google`、`/gradle-plugin`
> - 只打 arm64 可在 `src-tauri/gen/android/gradle.properties` 加 `abiList=arm64-v8a`、`archList=arm64`、`targetList=aarch64`

## 使用

1. 两端分别打开应用，点右上角齿轮填写 WebDAV 地址、用户名、密码 → 保存
2. 点「开始同步」
3. 任一端复制文字，另一端稍候即可粘贴

同步文件默认路径为 `/clipsync/clip-sync.json`（常量 `SYNC_FILE`，可在 `src-tauri/src/lib.rs` 中修改）。

## 已知限制

- 仅同步**纯文本**，图片与文件不支持
- **Android**：系统限制，仅当应用处于前台时才能读写剪贴板
- **macOS**：关闭窗口会隐藏到菜单栏继续运行
- 同步记录仅保存在内存（最近 100 条），退出即清空

## 安全

- WebDAV 账号密码保存在本机应用配置目录，**不会**写入项目文件
- 剪贴板内容经你的 WebDAV 传输，请确保使用 HTTPS，并避免同步密码、验证码等敏感信息
- 仓库中的占位符（如 `https://your-webdav-host`）仅为示例，请勿提交自己的真实地址与凭据

## License

MIT
