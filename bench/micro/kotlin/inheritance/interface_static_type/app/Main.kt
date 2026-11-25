package app

interface Repo {
    fun save()
}

class MemRepo : Repo {
    override fun save() {}
}

fun persist(r: Repo) {
    r.save()
}
