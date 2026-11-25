namespace App;

public class Animal
{
    public void Breathe()
    {
    }
}

public class Fish : Animal
{
}

public class Tank
{
    public void Keep(Fish fish)
    {
        fish.Breathe();
    }
}
