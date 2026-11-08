package com.wyj.clipsync

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import java.io.File

class MainActivity : TauriActivity() {
  /** 记录已处理过的 Intent，避免 onResume 重复提交同一张图 */
  private var handledIntent: Intent? = null
  private var notificationPermissionRequested = false
  private val activityToken = java.util.UUID.randomUUID().toString()
  private fun activityMarkerFile() = File(filesDir.parentFile, ".main_activity_alive")
  private fun markActivityAlive() {
    try {
      activityMarkerFile().writeText(activityToken)
    } catch (_: Throwable) {
      // 标记失败时 Rust 会暂停插件调用；生命周期回调本身不能因此崩溃。
    }
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Wry 在 Activity 销毁后调用 mobile plugin 会 panic；文件标记供 Rust 同步线程检查。
    markActivityAlive()
    handleShareIntent(intent)
  }

  override fun onResume() {
    super.onResume()
    markActivityAlive()
    SyncStatusService.start(this)
    if (Build.VERSION.SDK_INT >= 33 && !notificationPermissionRequested &&
      checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
      notificationPermissionRequested = true
      requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 42)
    }
    // 兜底：部分系统不会回调 onNewIntent
    handleShareIntent(intent)
  }

  override fun onDestroy() {
    try {
      val marker = activityMarkerFile()
      if (marker.exists() && marker.readText() == activityToken) marker.delete()
    } catch (_: Throwable) {
      // Rust 另有 panic 隔离，清理失败不能杀死系统服务进程。
    }
    super.onDestroy()
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    handleShareIntent(intent)
  }

  private fun handleShareIntent(intent: Intent?) {
    if (intent == null) return
    if (intent === handledIntent) return
    handledIntent = intent
    SharedImageHolder.offer(intent)
  }
}
