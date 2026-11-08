package com.wyj.clipsync

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.FileObserver
import android.os.IBinder
import androidx.core.app.NotificationCompat
import org.json.JSONObject
import java.io.File

/** 事件驱动的常驻状态通知，不做定时刷新。状态文件只含运行状态，不含剪贴板内容。 */
class SyncStatusService : Service() {
    private var observer: FileObserver? = null
    private var currentText: String? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        ensureChannel()
        showNotification(currentText ?: "正在读取同步状态…")
        if (observer == null) {
            val dir = filesDir.parentFile ?: return START_NOT_STICKY
            observer = object : FileObserver(dir, CLOSE_WRITE or MOVED_TO) {
                override fun onEvent(event: Int, path: String?) {
                    if (path == STATUS_FILE) refreshFromDisk()
                }
            }.also { it.startWatching() }
        }
        refreshFromDisk()
        // 不让状态服务脱离 Rust 同步引擎单独复活，避免显示过期“同步中”。
        return START_NOT_STICKY
    }

    private fun refreshFromDisk() {
        try {
            val file = File(filesDir.parentFile, STATUS_FILE)
            if (!file.exists()) return
            val status = JSONObject(file.readText())
            if (!status.optBoolean("running", false)) {
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
                return
            }
            val error = status.optString("error")
            val pending = status.optInt("pending", 0)
            val activityAlive = status.optBoolean("activityAlive", true)
            val text = when {
                error.isNotBlank() -> "连接异常 · 点击查看"
                pending > 0 -> "同步中 · 待发送 $pending"
                activityAlive -> "同步中 · 已连接"
                else -> "后台服务运行中"
            }
            if (text != currentText) showNotification(text)
        } catch (_: Throwable) {
            // 写入中的短暂不完整文件由下一次 FileObserver 事件重试。
        }
    }

    private fun showNotification(text: String) {
        currentText = text
        val openApp = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle("ClipSync")
            .setContentText(text)
            .setContentIntent(openApp)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .build()
        startForeground(NOTIFICATION_ID, notification)
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = getSystemService(NotificationManager::class.java)
        val channel = NotificationChannel(CHANNEL_ID, "同步状态", NotificationManager.IMPORTANCE_LOW)
        channel.description = "显示 ClipSync 当前同步状态"
        channel.setShowBadge(false)
        channel.enableVibration(false)
        channel.setSound(null, null)
        manager.createNotificationChannel(channel)
    }

    override fun onDestroy() {
        observer?.stopWatching()
        observer = null
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL_ID = "sync_status"
        private const val NOTIFICATION_ID = 7301
        private const val STATUS_FILE = ".sync_status.json"

        fun start(context: Context) {
            val intent = Intent(context, SyncStatusService::class.java)
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            } catch (_: Throwable) {
                // 系统限制后台启动时不能影响剪贴板和无障碍服务。
            }
        }
    }
}
