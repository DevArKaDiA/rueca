namespace App;

public class Formatter
{
    public string Format(string s)
    {
        return s;
    }

    public string Format(string s, int width)
    {
        return Format(s).PadLeft(width);
    }
}

public class View
{
    public string Show()
    {
        var f = new Formatter();
        return f.Format("a", 3);
    }
}
