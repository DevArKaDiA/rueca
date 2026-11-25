namespace App;

public class Wheel
{
    public void Spin()
    {
    }
}

public class Bike
{
    public Wheel Front { get; } = new Wheel();

    public void Ride()
    {
        Front.Spin();
        this.Front.Spin();
    }
}
