package com.wyj.clipsync

import android.accessibilityservice.AccessibilityService
import android.content.ClipboardManager
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.view.accessibility.AccessibilityEvent

/**
 * 无障碍服务：让 App 退到后台时也能读剪贴板（Android 10+ 限制普通应用后台读取）。
 * 只做一件事——周期性读取剪贴板文本，变化时放到 pending，交给 Rust 同步循环上传。
 */
object BackgroundClipboard {
    @Volatile
    private var pending: String? = null

    @Volatile
    var serviceEnabled: Boolean = false

    fun offer(text: String) {
        pending = text
    }

    fun consume(): String? {
        val v = pending
        pending = null
        return v
    }

    fun reset() {
        pending = null
    }
}

class ClipAccessibilityService : AccessibilityService() {

    private val handler = Handler(Looper.getMainLooper())
    private var lastText: String? = null

    private val poller = object : Runnable {
        override fun run() {
            try {
                val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                val clip = cm.primaryClip
                if (clip != null && clip.itemCount > 0) {
                    val text = clip.getItemAt(0)
                        ?.coerceToText(this@ClipAccessibilityService)
                        ?.toString()
                    if (!text.isNullOrEmpty() && text != lastText) {
                        lastText = text
                        BackgroundClipboard.offer(text)
                    }
                }
            } catch (_: Throwable) {
                // 某些场景（安全输入等）会抛异常，忽略即可
            }
            handler.postDelayed(this, POLL_MS)
        }
    }

    override fun onServiceConnected() {
        super.onServiceConnected()
        BackgroundClipboard.serviceEnabled = true
        handler.removeCallbacks(poller)
        handler.post(poller)
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        // 不需要监听事件，仅用轮询读剪贴板
    }

    override fun onInterrupt() {
        BackgroundClipboard.serviceEnabled = false
    }

    override fun onDestroy() {
        BackgroundClipboard.serviceEnabled = false
        handler.removeCallbacks(poller)
        super.onDestroy()
    }

    companion object {
        private const val POLL_MS = 1200L
    }
}
