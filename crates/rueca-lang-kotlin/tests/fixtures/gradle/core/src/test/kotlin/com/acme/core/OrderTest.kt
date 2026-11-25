package com.acme.core

import kotlin.test.Test
import kotlin.test.assertEquals

class OrderTest {
    @Test
    fun totals() {
        assertEquals(4L, Order(2).total())
    }
}
