package app

class Outer {
    fun base() {}

    inner class In {
        fun x() {
            base()
        }
    }
}
