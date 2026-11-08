package com.wyj.clipsync

import android.app.Notification
import android.content.Context
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import java.util.ArrayDeque

/** 内存中的验证码队列；不保存通知全文，也不把验证码写入应用日志。 */
object OtpInbox {
    private val pending = ArrayDeque<String>()

    @Volatile
    var serviceEnabled: Boolean = false

    @Volatile
    private var lastCode: String? = null

    @Volatile
    private var lastOfferedAt: Long = 0

    @Synchronized
    fun offer(code: String, now: Long = System.currentTimeMillis()) {
        // 同一条短信的通知可能连续更新；短时间内相同验证码只上传一次。
        if (code == lastCode && now - lastOfferedAt < 120_000) return
        lastCode = code
        lastOfferedAt = now
        if (pending.size >= 10) pending.removeFirst()
        pending.addLast(code)
    }

    @Synchronized
    fun consume(): String? = pending.pollFirst()

    @Synchronized
    fun reset() {
        pending.clear()
        lastCode = null
        lastOfferedAt = 0
    }
}

object OtpParser {
    private const val KEYWORDS =
        "验证码|校验码|动态码|动态密码|passcode|otp|one[-\\s]time(?:\\s+password)?|(?:verification|security|login|sign[-\\s]in|authentication)(?:\\s+code)?|(?:your|the)\\s+code"
    private val codeAfterKeyword =
        Regex("(?i)(?:$KEYWORDS)[^0-9]{0,16}([0-9]{4,8})(?![0-9])")
    private val codeBeforeKeyword =
        Regex("(?i)(?<![0-9])([0-9]{4,8})[^0-9]{0,16}(?:$KEYWORDS)")

    fun extract(text: String): String? {
        return codeAfterKeyword.find(text)?.groupValues?.get(1)
            ?: codeBeforeKeyword.find(text)?.groupValues?.get(1)
    }
}

class OtpNotificationService : NotificationListenerService() {
    override fun onListenerConnected() {
        super.onListenerConnected()
        OtpInbox.serviceEnabled = true
    }

    override fun onNotificationPosted(sbn: StatusBarNotification) {
        try {
            if (sbn.packageName == packageName) return
            val prefs = getSharedPreferences(ClipImagePlugin.SETTINGS_PREFS, Context.MODE_PRIVATE)
            if (!prefs.getBoolean(ClipImagePlugin.OTP_ENABLED, false)) return

            val extras = sbn.notification.extras
            val title = extras.getCharSequence(Notification.EXTRA_TITLE)?.toString().orEmpty()
            val text = extras.getCharSequence(Notification.EXTRA_TEXT)?.toString().orEmpty()
            val bigText = extras.getCharSequence(Notification.EXTRA_BIG_TEXT)?.toString().orEmpty()
            val content = listOf(title, text, bigText)
                .filter { it.isNotBlank() }
                .joinToString("\n")
            val code = OtpParser.extract(content) ?: return
            OtpInbox.offer(code)
        } catch (_: Throwable) {
            // 单条异常通知不能拖垮整个 App 进程及无障碍服务；不记录验证码或通知全文。
        }
    }

    override fun onListenerDisconnected() {
        OtpInbox.serviceEnabled = false
        super.onListenerDisconnected()
    }

    override fun onDestroy() {
        OtpInbox.serviceEnabled = false
        super.onDestroy()
    }
}
