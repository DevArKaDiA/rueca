package app

open class A {
    open fun f() {}
}

class B : A() {
    override fun f() {
        super.f()
    }
}
