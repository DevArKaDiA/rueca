namespace App;

public class Engine
{
    public void Start()
    {
    }
}

public class Car
{
    private readonly Engine _engine = new Engine();

    public void Drive()
    {
        _engine.Start();
    }
}
