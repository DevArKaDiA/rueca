package app

abstract class Task {
    abstract fun exec()

    fun run() {
        exec()
    }
}
