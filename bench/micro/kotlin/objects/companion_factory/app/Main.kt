package app

class Order private constructor(val id: Long) {
    companion object {
        fun create(id: Long): Order = Order(id)
    }
}

fun newOrder() = Order.create(1L)
