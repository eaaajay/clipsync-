package com.wyj.clipsync

import android.content.Intent
import android.net.Uri

/**
 * 接收系统「分享」进来的图片（支持单张与多张）。
 * MainActivity 把图片 URI 存到这里，由 ClipImagePlugin.takeSharedImage 逐张取走。
 */
object SharedImageHolder {
    private val queue = ArrayDeque<Uri>()

    fun offer(intent: Intent?) {
        if (intent == null) return
        val action = intent.action
        if (action != Intent.ACTION_SEND && action != Intent.ACTION_SEND_MULTIPLE) return
        val type = intent.type ?: return
        if (!type.startsWith("image/")) return

        if (action == Intent.ACTION_SEND) {
            @Suppress("DEPRECATION")
            val uri: Uri? = intent.getParcelableExtra(Intent.EXTRA_STREAM)
            if (uri != null) queue.addLast(uri)
        } else {
            @Suppress("DEPRECATION")
            val list: ArrayList<Uri>? = intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)
            list?.forEach { queue.addLast(it) }
        }
    }

    fun consume(): Uri? = if (queue.isEmpty()) null else queue.removeFirst()

    fun isEmpty(): Boolean = queue.isEmpty()
}
