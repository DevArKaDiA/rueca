package app

sealed interface Shape

class Circle : Shape {
    fun radius(): Double = 1.0
}

class Square : Shape {
    fun side(): Double = 2.0
}

fun size(s: Shape): Double = when (s) {
    is Circle -> s.radius()
    is Square -> s.side()
}
