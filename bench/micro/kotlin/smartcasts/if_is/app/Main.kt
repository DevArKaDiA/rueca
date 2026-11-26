package app

sealed interface Shape

class Circle : Shape {
    fun radius(): Double = 1.0
}

class Square : Shape {
    fun side(): Double = 2.0
}

fun maybe(x: Any): Double {
    if (x is Circle) {
        return x.radius()
    }
    return 0.0
}
