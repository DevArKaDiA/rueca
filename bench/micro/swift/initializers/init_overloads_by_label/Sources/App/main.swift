struct Temperature {
    var kelvin: Double

    init(celsius: Double) {
        kelvin = celsius + 273.15
    }

    init(fahrenheit: Double) {
        self.init(celsius: (fahrenheit - 32) / 1.8)
    }
}

func run() -> Double {
    return Temperature(fahrenheit: 50).kelvin
}

print(run())
