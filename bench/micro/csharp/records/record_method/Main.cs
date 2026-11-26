namespace App;

public record Temperature(double Celsius)
{
    public double Fahrenheit()
    {
        return Celsius * 9 / 5 + 32;
    }
}

public class Station
{
    public double Report(Temperature t)
    {
        return t.Fahrenheit();
    }
}
