def to_fahrenheit(celsius):
    value = celsius * 9 / 5 + 32
    return value


class Temperature:
    def __init__(self, celsius):
        self._celsius = celsius

    @property
    def fahrenheit(self):
        value = to_fahrenheit(self._celsius)
        return value

    @fahrenheit.setter
    def fahrenheit(self, value):
        celsius = self._to_celsius(value)
        self._celsius = celsius

    def _to_celsius(self, value):
        celsius = (value - 32) * 5 / 9
        return celsius

    def describe(self):
        self.fahrenheit = 50
        return f"{self.fahrenheit}F"
