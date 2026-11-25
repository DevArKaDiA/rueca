package app

open class Animal(val name: String)

class Dog : Animal("dog")

fun adopt(): Animal = Dog()
