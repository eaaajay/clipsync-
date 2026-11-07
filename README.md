# ClipSync 剪贴板同步

跨设备（macOS ↔ Android）的**剪贴板同步工具**，用你自己的 WebDAV 当转储点 —— 数据不经过任何第三方服务。

![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Android-blue)
![tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![rust](https://img.shields.io/badge/Rust-1.80+-orange?logo=rust)
![license](https://img.shields.io/badge/license-MIT-green)

> 手机和电脑之间复制粘贴，走你自己的 NAS，不依赖微信/输入法/第三方云。

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
- 启动后自动开始同步（已保存配置即生效）

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

macOS 的「辅助功能」授权（`Enter` 直接粘贴需要）绑定 App 的签名身份。若每次打包都用 ad-hoc 签名，签名哈希会变，授权就会失效、需要重新勾选。

解决办法：创建一次**固定的自签名证书**，并放进一个**专用钥匙串**（密码固定，避免每次 codesign 都弹授权框），之后所有版本都用它签名。

```bash
# 1. 生成证书（仅一次）
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem -days 3650 -nodes \
  -subj "/CN=ClipSync Dev" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "basicConstraints=critical,CA:false"

openssl pkcs12 -export -legacy -inkey key.pem -in cert.pem -out id.p12 \
  -passout pass:clipsync -name "ClipSync Dev"

# 2. 建专用钥匙串并导入（密码公开无妨，只为免交互）
KC="$HOME/Library/Keychains/clipsync-sign.keychain-db"
security create-keychain -p clipsync "$KC"
security set-keychain-settings -lut 21600 "$KC"
security unlock-keychain -p clipsync "$KC"
security import id.p12 -k "$KC" -P clipsync -T /usr/bin/codesign -A
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k clipsync "$KC"
security find-identity -v -p codesigning "$KC"   # 应能看到 "ClipSync Dev"
```

之后用脚本构建部署，会自动启用路径重映射并用该证书签名：

```bash
./scripts/deploy-macos.sh
```

首次运行后，到「系统设置 → 隐私与安全性 → 辅助功能」勾选 `ClipSync.app` 即可，以后升级无需重新授权（除非换证书）。

### Android

前置：JDK 17+、Android SDK、NDK。

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
# 仓库已包含 src-tauri/gen/android，通常无需再 init；仅重建工程时执行：
# npx tauri android init
npx tauri android build --apk
# 产物：src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

> **构建产物里别带本机路径**：Rust 会把源码绝对路径编进 `panic!` 信息，公开二进制会暴露本机用户名。
> 本项目用 `scripts/rustc-remap.sh`（`RUSTC_WRAPPER`）在编译时重映射成 `/build`：
> ```bash
> export RUSTC_WRAPPER="$PWD/scripts/rustc-remap.sh"
> ```
> 注意：**改完这个变量要 `cargo clean` 一次**，否则依赖会用缓存里的旧产物（仍含原路径）。

> 中国大陆网络环境提示：
> - rustup 组件下载失败时，可临时指定官方源 `RUSTUP_DIST_SERVER=https://static.rust-lang.org`
> - Gradle 拉取 `dl.google.com` / `repo.maven.apache.org` 出现 TLS 握手失败时，在 `src-tauri/gen/android/build.gradle.kts` 与 `buildSrc/build.gradle.kts` 的 `repositories` 中优先加入阿里云镜像：
>   `https://maven.aliyun.com/repository/public`、`/google`、`/gradle-plugin`
> - 只打 arm64 可在 `src-tauri/gen/android/gradle.properties` 加 `abiList=arm64-v8a`、`archList=arm64`、`targetList=aarch64`

## 安装

### Android
从 [Releases](https://github.com/eaaajay/clipsync-/releases) 下载 `ClipSync.apk`（arm64，约 9MB），
允许「安装未知应用」后安装。

### macOS
下载 Release 里的 `ClipSync.dmg`，拖到「应用程序」。

App 使用自签名证书（非 Apple 公证），首次打开会被 Gatekeeper 拦截，用以下任一方式放行：

```bash
# 方式一：终端去掉隔离属性
xattr -dr com.apple.quarantine /Applications/ClipSync.app

# 方式二：右键 App → 打开 → 仍要打开
```

首次使用「Enter 直接粘贴」需到 **系统设置 → 隐私与安全性 → 辅助功能** 勾选 ClipSync。

## 使用

1. 两端分别打开应用，点右上角齿轮填写 WebDAV 地址、用户名、密码 → 保存
2. 点「开始同步」（配置过之后每次启动会自动开始）
3. 任一端复制文字或图片，另一端稍候即可粘贴

### 数据放在哪
同步文件位于你 WebDAV 的 `/clipsync/` 目录：

| 文件 | 内容 |
|---|---|
| `clip-sync.json` | 最近一次同步的文本 |
| `favorites.json` | 收藏（多端合并，删除有墓碑） |
| `images/<hash>.<png\|jpg>` | 图片（按内容哈希命名，超过 7 天自动清理） |
| `images/index.json` | 图片索引（两端合并） |

### 快捷操作

| 操作 | 快捷键 |
|---|---|
| 呼出窗口 | `⌘⇧V`（可自定义） |
| 选择记录 | `↑` `↓` |
| 切换「最近/收藏」 | `←` `→` |
| 直接粘贴到光标处 | `Enter` |
| 隐藏窗口 | `Esc` |

## License

MIT
