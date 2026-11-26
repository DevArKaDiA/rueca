namespace App;

public interface IGreeter
{
    string Name();

    string Greet()
    {
        return "hi " + Name();
    }
}

public class English : IGreeter
{
    public string Name()
    {
        return "en";
    }
}
