package app

class Money(val cents: Long)

fun Money.format(): String = cents.toString()

fun show(m: Money): String = m.format()
