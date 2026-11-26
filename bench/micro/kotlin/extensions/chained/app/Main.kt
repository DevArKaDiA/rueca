package app

fun String.clean(): String = trim()

fun String.wrap(): String = "[" + this + "]"

fun tidy(s: String): String = s.clean().wrap()
