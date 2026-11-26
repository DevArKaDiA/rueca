package app

class Parser {
    fun parse(s: String) = normalize(s)

    companion object {
        fun normalize(s: String) = s.trim()
    }
}
