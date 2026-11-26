package app

fun greet(name: String, punct: String = "!"): String = name + punct

fun hello(): String = greet(name = "world")
