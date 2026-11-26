package app

enum class Color(val rgb: Int) {
    RED(0xff0000),
    GREEN(0x00ff00);

    fun hex(): String = rgb.toString(16)
}

fun red(): String = Color.RED.hex()
