package com.wyj.clipsync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class OtpParserTest {
    @Test
    fun extractsChineseVerificationCode() {
        assertEquals("123456", OtpParser.extract("【示例】验证码 123456，5 分钟内有效。"))
        assertEquals("8421", OtpParser.extract("动态密码为：8421，请勿泄露"))
        assertEquals("765432", OtpParser.extract("765432 是你的校验码"))
    }

    @Test
    fun extractsEnglishVerificationCode() {
        assertEquals("918273", OtpParser.extract("Your verification code is 918273."))
        assertEquals("042819", OtpParser.extract("042819 is your OTP."))
    }

    @Test
    fun ignoresUnrelatedNumbers() {
        assertNull(OtpParser.extract("订单金额 123456 元，明天 08:30 送达"))
        assertNull(OtpParser.extract("Meeting code is 123456789"))
        assertNull(OtpParser.extract("Meeting code is 123456"))
        assertNull(OtpParser.extract("验证码 123456789，请勿泄露"))
        assertNull(OtpParser.extract("123456789 是你的验证码"))
        assertNull(OtpParser.extract("验证码已发送，请注意查收"))
    }

    @Test
    fun keepsMultipleCodesInOrder() {
        OtpInbox.reset()
        OtpInbox.offer("111111", 1_000)
        OtpInbox.offer("222222", 2_000)
        assertEquals("111111", OtpInbox.consume())
        assertEquals("222222", OtpInbox.consume())
        assertNull(OtpInbox.consume())
    }

    @Test
    fun deduplicatesRepeatedNotificationUpdates() {
        OtpInbox.reset()
        OtpInbox.offer("123456", 1_000)
        OtpInbox.offer("123456", 2_000)
        assertEquals("123456", OtpInbox.consume())
        assertNull(OtpInbox.consume())
    }
}
