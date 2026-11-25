package app

fun helper() {}

fun repeatTwice(block: () -> Unit) {
    block()
    block()
}

fun go() = repeatTwice {
    helper()
}
