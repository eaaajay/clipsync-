# ClipSync 剪贴板同步

跨设备（macOS ↔ Android）的**剪贴板同步工具**，用你自己的 WebDAV 当转储点 —— 数据不经过任何第三方服务。

![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Android-blue)
![tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![rust](https://img.shields.io/badge/Rust-1.80+-orange?logo=rust)
![license](https://img.shields.io/badge/license-MIT-green)

> 手机和电脑之间复制粘贴，走你自己的 NAS，不依赖微信/输入法/第三方云。

## 截图

<!-- 截图占位：docs/screenshot.png -->
_（待补充）_

## 功能

**同步**
- 文字双向实时同步（WebDAV 轮询，可自适应降频）
- 图片 Mac → 手机：自动保存到相册 `Pictures/ClipSync`
- 图片 手机 → Mac：相册/微信选图 → **分享 → ClipSync**（安卓剪贴板不支持图片，故走系统分享）
- **离线队列**：断网时内容入队落盘，恢复后指数退避自动补传，重启不丢

**记录**
- 最近 7 天持久化保存（含图片缩略图），收藏永久保留并多端合并（删除有墓碑，不会回流）
- **使用即置顶**：粘贴/复制过的记录自动跳到第一位
- 搜索、JSON 工具（格式化/压缩/转义/去转义）
- 内容类型识别：网址一键打开、颜色色块预览、JWT / Base64 解码、时间戳转日期
- 图片记录点击看大图，一键复制或存相册

**macOS**
- 菜单栏常驻，实时显示最新一条记录
- 全局快捷键呼出（默认 `⌘⇧V`，可自定义）
- 键盘流：`↑↓` 选择、`←→` 切换「最近/收藏」、`Enter` **直接粘贴到光标处**、`Esc` 隐藏
- 开机自动同步

**Android**
- 前台同步；开启**无障碍服务**后可在后台持续同步
- 一键清理相册中本应用保存的图片

## 隐私与安全

- WebDAV 地址、账号只保存在本机，**密码存系统安全存储**（macOS 钥匙串 / Android Keystore 加密）
- 传输走你自己的 HTTPS WebDAV；仓库中不含任何个人域名、账号或密钥
- Android 无障碍服务仅用于**读取剪贴板文本**（`canRetrieveWindowContent=false`），不采集界面内容、不模拟操作
- 敏感内容（密码、验证码）建议不要经剪贴板同步，或临时关闭同步

## 已知限制

- Android 10+ 限制后台读取剪贴板，需开启无障碍服务才能后台同步
- Android 剪贴板不支持图片，图片方向为单向（Mac → 手机）+ 手机分享上传
- 仅同步文本与图片，不支持文件
- macOS「Enter 直接粘贴」需要「辅助功能」授权

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
