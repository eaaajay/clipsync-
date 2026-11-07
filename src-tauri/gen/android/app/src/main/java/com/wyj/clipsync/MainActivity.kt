package com.wyj.clipsync

import android.content.Intent
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  /** 记录已处理过的 Intent，避免 onResume 重复提交同一张图 */
  private var handledIntent: Intent? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    handleShareIntent(intent)
  }

  override fun onResume() {
    super.onResume()
    // 兜底：部分系统不会回调 onNewIntent
    handleShareIntent(intent)
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
