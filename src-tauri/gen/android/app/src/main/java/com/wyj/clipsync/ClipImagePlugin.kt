package com.wyj.clipsync

import android.app.Activity
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.FileOutputStream
import java.io.IOException

@InvokeArg
class UrlArgs {
    lateinit var url: String
}

@InvokeArg
class SecretArgs {
    lateinit var key: String
    lateinit var value: String
}

@InvokeArg
class SecretKeyArgs {
    lateinit var key: String
}

@InvokeArg
class SaveImageArgs {
    lateinit var base64: String
    var name: String? = null
}

@InvokeArg
class OtpEnabledArgs {
    var enabled: Boolean = false
}

/**
 * 把 PNG 保存到系统相册（Pictures/ClipSync），供 Android 端使用。
 * Android 剪贴板对图片的支持极差（输入法、IM 基本都不支持粘贴图片），
 * 因此改为「存相册 -> 从相册选取发送」。
 */
@TauriPlugin
class ClipImagePlugin(private val activity: Activity) : Plugin(activity) {
    companion object {
        const val SETTINGS_PREFS = "clipsync_settings"
        const val OTP_ENABLED = "otp_enabled"
    }

    /** 用 Android Keystore 派生的密钥加密存敏感信息（如 WebDAV 密码） */
    private fun secretPrefs(): SharedPreferences {
        val masterKey = MasterKey.Builder(activity)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        return EncryptedSharedPreferences.create(
            activity,
            "clipsync_secrets",
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM
        )
    }

    @Command
    fun openUrl(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(UrlArgs::class.java)
            val intent = Intent(Intent.ACTION_VIEW, Uri.parse(args.url))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: "无法打开链接")
        }
    }

    @Command
    fun refreshStatusService(invoke: Invoke) {
        SyncStatusService.start(activity)
        invoke.resolve()
    }

    /** 取走无障碍服务捕获的后台剪贴板文本 */
    @Command
    fun takeBackgroundClipboard(invoke: Invoke) {
        val text = BackgroundClipboard.consume()
        val res = JSObject()
        if (text == null) {
            res.put("empty", true)
        } else {
            res.put("empty", false)
            res.put("text", text)
        }
        invoke.resolve(res)
    }

    /** 无障碍服务是否已开启且真正连接（系统设置开启但服务崩溃时不能误报） */
    @Command
    fun isAccessibilityEnabled(invoke: Invoke) {
        val configured = try {
            val am = activity.getSystemService(Context.ACCESSIBILITY_SERVICE)
                as android.view.accessibility.AccessibilityManager
            am.getEnabledAccessibilityServiceList(android.accessibilityservice.AccessibilityServiceInfo.FEEDBACK_ALL_MASK)
                .any { it.resolveInfo.serviceInfo.packageName == activity.packageName }
        } catch (e: Exception) {
            false
        }
        val res = JSObject()
        res.put("enabled", configured && BackgroundClipboard.serviceEnabled)
        invoke.resolve(res)
    }

    /** 打开系统无障碍设置页 */
    @Command
    fun openAccessibilitySettings(invoke: Invoke) {
        try {
            val intent = android.content.Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS)
            intent.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: "无法打开设置")
        }
    }

    /** 取走通知服务捕获的验证码，仅保留在内存中直到同步线程取走 */
    @Command
    fun takeOtp(invoke: Invoke) {
        val code = OtpInbox.consume()
        val res = JSObject()
        res.put("empty", code == null)
        if (code != null) res.put("text", code)
        invoke.resolve(res)
    }

    @Command
    fun setOtpEnabled(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(OtpEnabledArgs::class.java)
            activity.getSharedPreferences(SETTINGS_PREFS, Context.MODE_PRIVATE)
                .edit()
                .putBoolean(OTP_ENABLED, args.enabled)
                .apply()
            if (!args.enabled) OtpInbox.reset()
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: "保存验证码同步设置失败")
        }
    }

    @Command
    fun getOtpStatus(invoke: Invoke) {
        val enabled = activity.getSharedPreferences(SETTINGS_PREFS, Context.MODE_PRIVATE)
            .getBoolean(OTP_ENABLED, false)
        val permissionGranted = androidx.core.app.NotificationManagerCompat
            .getEnabledListenerPackages(activity)
            .contains(activity.packageName)
        val res = JSObject()
        res.put("enabled", enabled)
        res.put("permissionGranted", permissionGranted)
        res.put("connected", OtpInbox.serviceEnabled)
        invoke.resolve(res)
    }

    @Command
    fun openNotificationSettings(invoke: Invoke) {
        try {
            val intent = Intent(android.provider.Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            activity.startActivity(intent)
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: "无法打开通知使用权设置")
        }
    }

    @Command
    fun setSecret(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(SecretArgs::class.java)
            secretPrefs().edit().putString(args.key, args.value).apply()
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: "保存密钥失败")
        }
    }

    @Command
    fun getSecret(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(SecretKeyArgs::class.java)
            val res = JSObject()
            res.put("value", secretPrefs().getString(args.key, "") ?: "")
            invoke.resolve(res)
        } catch (e: Exception) {
            invoke.reject(e.message ?: "读取密钥失败")
        }
    }

    /** 取走「分享」进来的图片，转成 PNG（长边不超过 2560）并返回 base64 */
    @Command
    fun takeSharedImage(invoke: Invoke) {
        val uri = SharedImageHolder.consume()
        val res = JSObject()
        if (uri == null) {
            res.put("empty", true)
            invoke.resolve(res)
            return
        }
        try {
            val resolver = activity.contentResolver
            val original = resolver.openInputStream(uri)?.use { input ->
                BitmapFactory.decodeStream(input)
            } ?: throw IOException("无法读取图片")

            val maxSide = 2560
            val scale = minOf(
                1.0f,
                maxSide.toFloat() / maxOf(original.width, original.height)
            )
            val bitmap = if (scale < 1f) {
                Bitmap.createScaledBitmap(
                    original,
                    (original.width * scale).toInt().coerceAtLeast(1),
                    (original.height * scale).toInt().coerceAtLeast(1),
                    true
                )
            } else {
                original
            }

            val out = java.io.ByteArrayOutputStream()
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
            val bytes = out.toByteArray()

            res.put("empty", false)
            res.put("base64", Base64.encodeToString(bytes, Base64.NO_WRAP))
            res.put("name", queryDisplayName(uri))
            res.put("width", bitmap.width)
            res.put("height", bitmap.height)
            invoke.resolve(res)
        } catch (e: Exception) {
            invoke.reject(e.message ?: "读取分享图片失败")
        }
    }

    private fun queryDisplayName(uri: Uri): String {
        return try {
            activity.contentResolver.query(uri, null, null, null, null)?.use { c ->
                val idx = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
                if (idx >= 0 && c.moveToFirst()) c.getString(idx) else null
            } ?: "shared.png"
        } catch (e: Exception) {
            "shared.png"
        }
    }

    @Command
    fun saveToGallery(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(SaveImageArgs::class.java)
            val bytes = Base64.decode(args.base64, Base64.NO_WRAP)
            val fileName = (args.name ?: ("clipsync_" + System.currentTimeMillis())) + ".png"

            val uri: Uri = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                saveWithMediaStore(bytes, fileName)
            } else {
                saveLegacy(bytes, fileName)
            }

            val res = JSObject()
            res.put("uri", uri.toString())
            res.put("name", fileName)
            invoke.resolve(res)
        } catch (e: Exception) {
            invoke.reject(e.message ?: "保存到相册失败")
        }
    }

    @Command
    fun clearGallery(invoke: Invoke) {
        var deleted = 0
        try {
            val resolver = activity.contentResolver

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                // Android 10+：按 relative_path 精确匹配本 App 的相簿
                val selection = MediaStore.Images.Media.RELATIVE_PATH + " LIKE ?"
                val selArgs = arrayOf(Environment.DIRECTORY_PICTURES + "/ClipSync%")
                deleted = resolver.delete(
                    MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                    selection,
                    selArgs
                )
            } else {
                val dir = File(
                    Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_PICTURES),
                    "ClipSync"
                )
                dir.listFiles()?.forEach { f ->
                    if (f.name.startsWith("clipsync_") && f.delete()) deleted++
                }
                android.media.MediaScannerConnection.scanFile(
                    activity,
                    arrayOf(dir.absolutePath),
                    arrayOf("image/png"),
                    null
                )
            }

            val res = JSObject()
            res.put("deleted", deleted)
            invoke.resolve(res)
        } catch (e: Exception) {
            invoke.reject(e.message ?: "清理相册失败")
        }
    }

    private fun saveWithMediaStore(bytes: ByteArray, fileName: String): Uri {
        val resolver = activity.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, fileName)
            put(MediaStore.Images.Media.MIME_TYPE, "image/png")
            put(
                MediaStore.Images.Media.RELATIVE_PATH,
                Environment.DIRECTORY_PICTURES + "/ClipSync"
            )
            put(MediaStore.Images.Media.IS_PENDING, 1)
        }

        val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)
            ?: throw IOException("无法创建相册条目")

        try {
            resolver.openOutputStream(uri)?.use { out ->
                out.write(bytes)
                out.flush()
            } ?: throw IOException("无法写入相册")
        } finally {
            values.clear()
            values.put(MediaStore.Images.Media.IS_PENDING, 0)
            resolver.update(uri, values, null, null)
        }
        return uri
    }

    @Suppress("DEPRECATION")
    private fun saveLegacy(bytes: ByteArray, fileName: String): Uri {
        val dir = File(
            Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_PICTURES),
            "ClipSync"
        )
        if (!dir.exists() && !dir.mkdirs()) {
            throw IOException("无法创建目录")
        }
        val file = File(dir, fileName)
        FileOutputStream(file).use { out ->
            out.write(bytes)
            out.flush()
        }
        android.media.MediaScannerConnection.scanFile(
            activity,
            arrayOf(file.absolutePath),
            arrayOf("image/png"),
            null
        )
        return Uri.fromFile(file)
    }
}
