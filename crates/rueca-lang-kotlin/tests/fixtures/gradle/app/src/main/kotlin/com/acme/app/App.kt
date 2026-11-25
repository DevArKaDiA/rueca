package com.acme.app

import com.acme.core.LegacyPricer
import com.acme.core.Order
import com.acme.core.Repo
import com.acme.core.total
import com.acme.core.text.shout

class App(private val repo: Repo) {
    fun checkout(id: Long): Long {
        val order = Order.create(id)
        repo.save(order)
        val direct = Order(id, 3)
        val port = System.getenv("SHOP_PORT")
        "done".shout()
        return LegacyPricer().price(direct) + order.total()
    }
}
