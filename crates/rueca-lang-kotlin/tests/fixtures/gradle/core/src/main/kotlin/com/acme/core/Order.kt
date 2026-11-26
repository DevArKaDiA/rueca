package com.acme.core

/** An order line total. */
data class Order(val id: Long, val qty: Int = 1) {
    fun price(): Long = id * qty

    companion object {
        fun create(id: Long): Order = Order(id)
    }
}

fun Order.total(): Long = price() * 2
