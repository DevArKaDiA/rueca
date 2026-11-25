import Lib

extension Vector {
    func scaled(_ k: Double) -> Vector {
        return Vector(x: x * k)
    }
}

func run() -> Double {
    return Vector(x: 1).scaled(2).x
}

print(run())
