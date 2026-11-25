package app

fun Int.label(): String = "int"

fun String.label(): String = "string"

fun describe(s: String): String = s.label()
