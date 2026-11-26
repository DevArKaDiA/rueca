package com.acme.core

interface Repo {
    fun save(order: Order)
}

class MemRepo : Repo {
    private val orders = mutableListOf<Order>()

    override fun save(order: Order) {
        orders.add(order)
    }
}
