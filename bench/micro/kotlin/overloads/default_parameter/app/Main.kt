package app

class Query {
    fun find(id: Int, limit: Int = 10) {}

    fun find(name: String) {}
}

fun lookup(q: Query) {
    q.find(1)
}
