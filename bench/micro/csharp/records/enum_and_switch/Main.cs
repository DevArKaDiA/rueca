namespace App;

public enum Color
{
    Red,
    Green,
}

public class Painter
{
    public string Name(Color c)
    {
        return c switch
        {
            Color.Red => Red(),
            _ => "other",
        };
    }

    private string Red()
    {
        return "red";
    }
}
